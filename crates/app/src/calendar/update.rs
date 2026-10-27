//! Updates, on the window's side: looking for one when backfill opens, the header button
//! that restarts into it, and installing it when backfill quits instead.

use std::{
    cell::RefCell,
    path::PathBuf,
    rc::Rc,
    time::{Duration, Instant},
};

use backfill_http::Http;
use backfill_update::{Updater, Version};
use gpui::{
    Animation, AnimationExt, App, Context, FontWeight, IntoElement, SharedString, div, ease_out_quint, millis,
    prelude::*, px,
};

use super::Backfill;
use crate::{
    settings_panel::Status,
    theme::{Theme, with_alpha},
};

/// Only release builds update themselves, since `scripts/package-macos.sh` turns this on.
/// A build from source would otherwise replace itself with the latest release.
pub const ENABLED: bool = cfg!(all(feature = "updates", target_os = "macos"));
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How long backfill can stay open before it looks again, when the window comes back.
const RECHECK: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Update {
    /// Not looked for, because looking when backfill opens is off.
    #[default]
    Idle,
    Checking,
    Downloading(Version),
    UpToDate,
    /// Downloaded and checked. It's installed on restart, or when backfill quits.
    Ready(Version),
    Failed(SharedString),
}

/// The downloaded app and the app it replaces. Shared with the quit hook, which outlives
/// the window: closing the window is what quits backfill.
#[derive(Clone, Default)]
pub struct Pending(pub(super) Rc<RefCell<Option<(PathBuf, PathBuf)>>>);

impl Pending {
    fn take(&self) -> Option<(PathBuf, PathBuf)> {
        self.0.borrow_mut().take()
    }
}

impl Backfill {
    /// Once, when backfill opens: installs a downloaded update when it quits, and looks for
    /// one if that's on.
    pub(super) fn start_updates(&mut self, cx: &mut Context<Self>) {
        if !ENABLED {
            return;
        }
        let pending = self.pending_update.clone();
        App::on_app_quit(cx, move |_| {
            if let Some((staged, app)) = pending.take() {
                backfill_update::install_after_quit(&staged, &app, false).ok();
            }
            async {}
        })
        .detach();
        if self.settings.check_for_updates {
            self.check_for_updates(cx);
        }
    }

    /// Turning checks off also drops an update that's on its way or waiting, so nothing is
    /// installed that wasn't asked for. Check now finds it again without downloading it.
    pub(super) fn set_check_for_updates(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.check_for_updates = on;
        if on {
            self.check_for_updates(cx);
        } else {
            self.update_task = None;
            self.pending_update.take();
            self.set_update(Update::Idle, cx);
        }
    }

    /// Looks again when the window comes back, if backfill has been open a long while.
    pub(super) fn recheck_updates(&mut self, cx: &mut Context<Self>) {
        let due = self.update_checked.is_none_or(|at| at.elapsed() > RECHECK);
        if self.settings.check_for_updates && due && matches!(self.update, Update::UpToDate | Update::Failed(_)) {
            self.check_for_updates(cx);
        }
    }

    /// Looks for a newer release, and downloads it in the background.
    pub(super) fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        if !ENABLED || matches!(self.update, Update::Checking | Update::Downloading(_) | Update::Ready(_)) {
            return;
        }
        let Some(current) = Version::parse(VERSION) else { return };
        let updater = match cx.app_path() {
            Ok(app) if app.extension().is_some_and(|e| e == "app") => Updater {
                app,
                folder: dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("backfill/updates"),
            },
            _ => {
                self.set_update(Update::Failed("backfill updates only when it runs as an app".into()), cx);
                return;
            }
        };
        if let Err(error) = updater.can_replace() {
            self.set_update(Update::Failed(error.into()), cx);
            return;
        }
        // Another list of releases, for trying updates out.
        let feed = std::env::var("BACKFILL_UPDATE_FEED").unwrap_or_else(|_| backfill_update::FEED.into());
        self.update_checked = Some(Instant::now());
        self.set_update(Update::Checking, cx);
        self.update_task = Some(cx.spawn(async move |this, cx| {
            let (found, updater) = cx
                .background_spawn(async move {
                    let found = backfill_update::newer(&Http::default(), &feed, current);
                    if let Ok(None) = found {
                        updater.clear();
                    }
                    (found, updater)
                })
                .await;
            let release = match found {
                Ok(Some(release)) => release,
                done => {
                    let update = match done {
                        Err(error) => Update::Failed(error.into()),
                        _ => Update::UpToDate,
                    };
                    this.update(cx, |this, cx| this.set_update(update, cx)).ok();
                    return;
                }
            };
            let version = release.version;
            this.update(cx, |this, cx| this.set_update(Update::Downloading(version), cx)).ok();
            let (staged, app) =
                cx.background_spawn(async move { (updater.prepare(&release, &Http::default()), updater.app) }).await;
            this.update(cx, |this, cx| match staged {
                Ok(staged) => {
                    *this.pending_update.0.borrow_mut() = Some((staged, app));
                    this.set_update(Update::Ready(version), cx);
                }
                Err(error) => this.set_update(Update::Failed(error.into()), cx),
            })
            .ok();
        }));
    }

    fn set_update(&mut self, update: Update, cx: &mut Context<Self>) {
        self.update = update;
        self.push_statuses(cx);
    }

    /// Quits, puts the downloaded version in place and opens it.
    pub(super) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        // Quitting halfway through would leave Jira with changes backfill doesn't know it made.
        if self.sheet.as_ref().is_some_and(|s| s.running) {
            self.show_toast("backfill can restart once the sync is done", false, cx);
            return;
        }
        let Some((staged, app)) = self.pending_update.take() else { return };
        if self.dirty {
            self.persist();
        }
        match backfill_update::install_after_quit(&staged, &app, true) {
            Ok(()) => cx.quit(),
            Err(error) => self.set_update(Update::Failed(error.into()), cx),
        }
    }

    /// How updates are doing, for Settings.
    pub(super) fn update_status(&self) -> Status {
        match &self.update {
            Update::Idle => Status::Off,
            Update::Checking => Status::Working("Looking for a newer version".into()),
            Update::Downloading(version) => Status::Working(format!("Downloading backfill {version}").into()),
            Update::UpToDate => Status::Ok("This is the newest version".into()),
            Update::Ready(version) => Status::Ok(format!("backfill {version} is ready to install").into()),
            Update::Failed(error) => Status::Problem(error.clone()),
        }
    }

    /// The header's "Restart to update" button, once an update is ready.
    pub(super) fn render_update_button(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let Update::Ready(version) = self.update else { return None };
        Some(
            div()
                .id("restart-to-update")
                .occlude()
                .flex()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .rounded_lg()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.focus)
                .bg(with_alpha(theme.focus, 0.12))
                .cursor_pointer()
                .hover(|s| s.bg(with_alpha(theme.focus, 0.22)))
                .on_click(cx.listener(|this, _, _, cx| this.restart_to_update(cx)))
                .child(div().size(px(7.)).rounded_full().bg(theme.focus))
                .child("Restart to update")
                .child(div().text_xs().text_color(with_alpha(theme.focus, 0.7)).child(version.to_string()))
                .with_animation("update-ready", Animation::new(millis(320)).with_easing(ease_out_quint()), |d, t| {
                    d.opacity(t)
                }),
        )
    }
}
