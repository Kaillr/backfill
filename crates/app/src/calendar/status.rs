//! How the connections are doing, as one quiet item in the header: "Synced 14:14" while all
//! is well, "Syncing…" while something is under way, and which one needs you when something
//! is wrong. Clicking it shows each connection, with the way to Sync now and to Settings.

use gpui::{
    Anchor, Animation, AnimationExt, AnyElement, Bounds, Context, FontWeight, IntoElement, MouseButton, Pixels,
    SharedString, anchored, deferred, div, ease_out_quint, millis, point, prelude::*, px,
};

use super::Backfill;
use crate::{
    settings::{Pane, Status},
    theme::{Theme, with_alpha},
};

/// The popover, while it's open.
pub(super) struct Popover {
    /// Counts popovers opened, so each one plays its entrance.
    pub generation: usize,
}

/// What the header says, all connections taken together.
enum Summary {
    /// One of them has a problem, the first in the order they're listed.
    Attention(&'static str),
    Syncing,
    Synced,
}

impl Backfill {
    fn connections(&self) -> [(Pane, &'static str, &Status); 4] {
        [
            (Pane::Jira, "Jira", &self.statuses.jira),
            (Pane::Toggl, "Toggl", &self.statuses.toggl),
            (Pane::Calendar, "Outlook", &self.statuses.calendar),
            (Pane::Code, "Git", &self.statuses.git),
        ]
    }

    fn summary(&self) -> Summary {
        let connections = self.connections();
        if let Some((_, name, _)) = connections.iter().find(|(_, _, s)| matches!(s, Status::Problem(_))) {
            Summary::Attention(name)
        } else if connections.iter().any(|(_, _, s)| matches!(s, Status::Working(_))) {
            Summary::Syncing
        } else {
            Summary::Synced
        }
    }

    pub(super) fn toggle_status(&mut self, cx: &mut Context<Self>) {
        if self.status_popover.take().is_none() {
            self.status_opened += 1;
            self.status_popover = Some(Popover { generation: self.status_opened });
        }
        cx.notify();
    }

    pub(super) fn close_status(&mut self, cx: &mut Context<Self>) {
        if self.status_popover.take().is_some() {
            cx.notify();
        }
    }

    /// The header's item. Before Jira is connected there's nothing to say: the header's
    /// Connect Jira button says it.
    pub(super) fn render_status(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.store.is_some() && !self.jira_configured() {
            return None;
        }
        let (dot, text, attention): (_, SharedString, _) = match self.summary() {
            Summary::Attention(name) => (theme.danger, format!("{name} needs attention").into(), true),
            Summary::Syncing => (theme.focus, "Syncing…".into(), false),
            Summary::Synced => (
                theme.ok,
                match self.synced_at {
                    Some(at) => format!("Synced {}", self.fmt_time(at)).into(),
                    None => "Up to date".into(),
                },
                false,
            ),
        };
        let open = self.status_popover.is_some();
        let dot = div().size(px(7.)).rounded_full().bg(dot);
        let dot = match self.summary() {
            // A slow pulse while something is under way.
            Summary::Syncing => dot
                .with_animation(
                    "syncing",
                    Animation::new(millis(1100)).repeat().with_easing(|t| 0.5 - 0.5 * (t * std::f32::consts::TAU).cos()),
                    |d, t| d.opacity(0.35 + 0.65 * t),
                )
                .into_any_element(),
            _ => dot.into_any_element(),
        };
        Some(
            div()
                .id("sync-status")
                .occlude()
                .flex()
                .items_center()
                .gap_2()
                .px_2p5()
                .py_1()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .when(attention, |d| d.bg(with_alpha(theme.danger, 0.12)).text_color(theme.danger))
                .when(!attention, |d| d.text_color(theme.muted))
                .when(open, |d| d.bg(theme.line))
                .hover(|s| s.bg(theme.line))
                .on_prepaint(cx.listener(|this, prepaint: &gpui::InteractivityPrepaint, _, _| this.status_bounds = Some(prepaint.bounds)))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_status(cx)))
                .child(dot)
                .child(text)
                .into_any_element(),
        )
    }

    /// Each connection and how it's doing, under the header's item. Each opens its part of
    /// Settings.
    pub(super) fn render_status_popover(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let popover = self.status_popover.as_ref()?;
        let at: Bounds<Pixels> = self.status_bounds?;
        let rows = self.connections().map(|(pane, name, status)| {
            let text: SharedString = match status {
                Status::Off if pane == Pane::Calendar => "Not available".into(),
                Status::Off if pane == Pane::Code => "No folders chosen".into(),
                Status::Off => "Not connected".into(),
                Status::Working(t) | Status::Ok(t) | Status::Problem(t) => t.clone(),
            };
            div()
                .id(("status-row", pane as usize))
                .flex()
                .items_start()
                .gap_2p5()
                .mx_1()
                .px_2p5()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme.line))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.close_status(cx);
                    this.open_settings(Some(pane), cx);
                }))
                .child(div().mt(px(5.)).size(px(7.)).flex_shrink_0().rounded_full().bg(status.color(theme)))
                .child(div().w(px(58.)).flex_shrink_0().font_weight(FontWeight::MEDIUM).child(name))
                .child(
                    div()
                        .flex_1()
                        // Or the text sets its width, unwrapped, and runs out of the popover.
                        .min_w(px(0.))
                        .text_color(match status {
                            Status::Problem(_) => theme.danger,
                            Status::Off => theme.faint,
                            _ => theme.muted,
                        })
                        .child(text),
                )
                .into_any_element()
        });
        let action = |id: &'static str, label: &'static str, key: &'static str| {
            div()
                .id(id)
                .flex()
                .items_center()
                .mx_1()
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .cursor_pointer()
                .hover(|s| s.bg(theme.line))
                .child(div().flex_1().child(label))
                .child(div().text_xs().text_color(theme.faint).child(key))
        };
        let element = div()
            .id("sync-status-popover")
            .occlude()
            .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                // The header's item toggles it itself.
                if !this.status_bounds.is_some_and(|b| b.contains(&event.position)) {
                    this.close_status(cx);
                }
            }))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .w(px(360.))
            .py_1()
            .flex()
            .flex_col()
            .rounded_lg()
            .bg(theme.raised)
            .border_1()
            .border_color(theme.line_strong)
            .shadow_xl()
            .text_sm()
            .children(rows)
            .child(div().mx_2().my_1().h(px(1.)).bg(theme.line))
            .child(action("status-sync-now", "Sync now", "⌘R").on_click(cx.listener(|this, _, window, cx| {
                this.close_status(cx);
                this.sync_now(&super::SyncNow, window, cx);
            })))
            .child(action("status-settings", "Settings…", "⌘,").on_click(cx.listener(|this, _, _, cx| {
                this.close_status(cx);
                this.open_settings(None, cx);
            })))
            .with_animation(("sync-status-popover", popover.generation), Animation::new(millis(110)).with_easing(ease_out_quint()), |d, t| {
                d.opacity(t).mt(px(-3. * (1. - t)))
            });
        Some(
            deferred(
                anchored()
                    .anchor(Anchor::TopRight)
                    .position(point(at.right(), at.bottom() + px(6.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(element),
            )
            .priority(2)
            .into_any_element(),
        )
    }
}
