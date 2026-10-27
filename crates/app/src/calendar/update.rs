//! Updates, on the window's side: looking for one when backfill opens, the header button
//! that restarts into it, and installing it when backfill quits instead.
//!
//! Velopack does the work. The installer puts its updater inside the app, and each release
//! on GitHub carries a full package and a delta from the one before, so an update usually
//! downloads only what changed. Builds from source have no updater and never update.

use std::{
    cell::RefCell,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt, App, Context, FontWeight, IntoElement, SharedString, div, ease_out_quint, millis,
    prelude::*, px,
};
use velopack::{UpdateCheck, UpdateManager, VelopackAsset, sources};

use super::Backfill;
use crate::{
    settings_panel::Status,
    theme::{Theme, with_alpha},
};

const REPO: &str = "https://github.com/Kaillr/backfill";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// How long backfill can stay open before it looks again, when the window comes back.
const RECHECK: Duration = Duration::from_secs(12 * 60 * 60);

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Update {
    /// Not installed with the installer, such as a build from source.
    #[default]
    Unavailable,
    /// Not looked for, because looking when backfill opens is off.
    Idle,
    Checking,
    Downloading(SharedString),
    UpToDate,
    /// Downloaded. It's installed on restart, or when backfill quits.
    Ready(SharedString),
    Failed(SharedString),
}

/// The downloaded update. Shared with the quit hook, which outlives the window: closing the
/// window is what quits backfill.
#[derive(Clone, Default)]
pub struct Pending(Rc<RefCell<Option<(UpdateManager, VelopackAsset)>>>);

impl Pending {
    fn take(&self) -> Option<(UpdateManager, VelopackAsset)> {
        self.0.borrow_mut().take()
    }
}

/// The updater, if backfill was installed with one. `BACKFILL_UPDATE_FEED` points it at
/// another feed, a folder or a URL, for trying updates out.
fn updater() -> Option<UpdateManager> {
    let source: Box<dyn sources::UpdateSource> = match std::env::var("BACKFILL_UPDATE_FEED") {
        Ok(feed) if feed.starts_with("http") => Box::new(sources::HttpSource::new(feed)),
        Ok(feed) => Box::new(sources::FileSource::new(feed)),
        // While backfill is 0.x every release is a beta, marked as a pre-release on GitHub.
        Err(_) => Box::new(sources::GithubSource::new(REPO, None, VERSION.starts_with("0."))),
    };
    UpdateManager::new_boxed(source, None, None).ok()
}

impl Backfill {
    /// Once, when backfill opens: installs a downloaded update when it quits, and looks for
    /// one if that's on.
    pub(super) fn start_updates(&mut self, cx: &mut Context<Self>) {
        let Some(updater) = updater() else { return };
        self.updater = Some(updater);
        let pending = self.pending_update.clone();
        App::on_app_quit(cx, move |_| {
            if let Some((updater, asset)) = pending.take() {
                updater.wait_exit_then_apply_updates(&asset, true, false, Vec::<String>::new()).ok();
            }
            async {}
        })
        .detach();
        self.set_update(Update::Idle, cx);
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
        } else if self.updater.is_some() {
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

    /// Looks for a newer release, and downloads it in the background. One downloaded
    /// before is ready straight away.
    pub(super) fn check_for_updates(&mut self, cx: &mut Context<Self>) {
        let Some(updater) = self.updater.clone() else { return };
        if matches!(self.update, Update::Checking | Update::Downloading(_) | Update::Ready(_)) {
            return;
        }
        if let Some(asset) = updater.get_update_pending_restart() {
            self.set_ready(updater, asset, cx);
            return;
        }
        self.update_checked = Some(Instant::now());
        self.set_update(Update::Checking, cx);
        self.update_task = Some(cx.spawn(async move |this, cx| {
            let check = cx.background_spawn({
                let updater = updater.clone();
                async move { updater.check_for_updates() }
            });
            let update = match check.await {
                Ok(UpdateCheck::UpdateAvailable(update)) => update,
                done => {
                    let update = match done {
                        Err(error) => Update::Failed(format!("Couldn't look for updates: {error}").into()),
                        _ => Update::UpToDate,
                    };
                    this.update(cx, |this, cx| this.set_update(update, cx)).ok();
                    return;
                }
            };
            let version = SharedString::from(update.TargetFullRelease.Version.clone());
            this.update(cx, |this, cx| this.set_update(Update::Downloading(version), cx)).ok();
            let download = cx.background_spawn({
                let (updater, update) = (updater.clone(), update.clone());
                async move { updater.download_updates(&update, None) }
            });
            let downloaded = download.await;
            this.update(cx, |this, cx| match downloaded {
                Ok(()) => this.set_ready(updater, update.TargetFullRelease, cx),
                Err(error) => this.set_update(Update::Failed(format!("Couldn't download the update: {error}").into()), cx),
            })
            .ok();
        }));
    }

    fn set_ready(&mut self, updater: UpdateManager, asset: VelopackAsset, cx: &mut Context<Self>) {
        let version = SharedString::from(asset.Version.clone());
        *self.pending_update.0.borrow_mut() = Some((updater, asset));
        self.set_update(Update::Ready(version), cx);
    }

    fn set_update(&mut self, update: Update, cx: &mut Context<Self>) {
        self.update = update;
        self.push_statuses(cx);
    }

    /// Quits, and the updater puts the new version in place and opens it.
    pub(super) fn restart_to_update(&mut self, cx: &mut Context<Self>) {
        // Quitting halfway through would leave Jira with changes backfill doesn't know it made.
        if self.sheet.as_ref().is_some_and(|s| s.running) {
            self.show_toast("backfill can restart once the sync is done", false, cx);
            return;
        }
        let Some((updater, asset)) = self.pending_update.take() else { return };
        if self.dirty {
            self.persist();
        }
        match updater.wait_exit_then_apply_updates(&asset, false, true, Vec::<String>::new()) {
            Ok(()) => cx.quit(),
            Err(error) => self.set_update(Update::Failed(format!("Couldn't start the update: {error}").into()), cx),
        }
    }

    /// How updates are doing, for Settings.
    pub(super) fn update_status(&self) -> Status {
        match &self.update {
            Update::Unavailable | Update::Idle => Status::Off,
            Update::Checking => Status::Working("Looking for a newer version".into()),
            Update::Downloading(version) => Status::Working(format!("Downloading backfill {version}").into()),
            Update::UpToDate => Status::Ok("This is the newest version".into()),
            Update::Ready(version) => Status::Ok(format!("backfill {version} is ready to install").into()),
            Update::Failed(error) => Status::Problem(error.clone()),
        }
    }

    /// The header's "Restart to update" button, once an update is ready.
    pub(super) fn render_update_button(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let Update::Ready(version) = &self.update else { return None };
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
                .child(div().text_xs().text_color(with_alpha(theme.focus, 0.7)).child(version.clone()))
                .with_animation("update-ready", Animation::new(millis(320)).with_easing(ease_out_quint()), |d, t| {
                    d.opacity(t)
                }),
        )
    }
}
