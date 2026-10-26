//! The review sheet: every change a sync would make in Jira, before anything is written.

use backfill_core::{IssueKey, sync::SyncOp, time::{format_hours, local_date}};
use gpui::{
    Animation, AnimationExt, AnyElement, Context, FontWeight, MouseButton, SharedString, Window,
    div, ease_out_quint, millis, prelude::*, px,
};
use jiff::{SignedDuration, civil::Date};

use super::{ApplySync, Backfill, Check, OpStatus, SHEET_W, day_name, month};
use crate::{picker, theme::Theme};

impl Backfill {
    /// Drops one change from the sync: what Jira has stays as it is, and the calendar goes
    /// back to showing it. Undo (⌘Z) brings the change back.
    pub(super) fn undo_change(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(op) = self.sheet.as_ref().filter(|s| !s.running && !s.finished).and_then(|s| s.ops.get(index).cloned()) else {
            return;
        };
        self.checkpoint();
        self.ws.revert(&op);
        let ops = self.ws.sync_plan();
        if ops.is_empty() {
            self.close_sheet(window, cx);
            self.show_toast("Everything here matches Jira", true, cx);
            return;
        }
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.status = vec![OpStatus::Waiting; ops.len()];
            sheet.ops = ops;
        }
        cx.notify();
    }

    pub(super) fn render_sheet(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let sheet = self.sheet.as_ref().expect("sheet is open");
        let mut created = 0;
        let mut updated = 0;
        let mut deleted = 0;
        for op in &sheet.ops {
            match op {
                SyncOp::Create(_) => created += 1,
                SyncOp::Update { .. } | SyncOp::Move { .. } => updated += 1,
                SyncOp::Delete(_) => deleted += 1,
            }
        }
        let net: SignedDuration = sheet.ops.iter().map(SyncOp::delta).sum();
        let mut parts = Vec::new();
        if created > 0 {
            parts.push(format!("{created} new"));
        }
        if updated > 0 {
            parts.push(format!("{updated} changed"));
        }
        if deleted > 0 {
            parts.push(format!("{deleted} deleted"));
        }
        let net_label = if net.is_zero() {
            "no change in total hours".to_owned()
        } else if net.is_positive() {
            format!("{} more logged", format_hours(net))
        } else {
            format!("{} less logged", format_hours(-net))
        };

        let mut last_day: Option<Date> = None;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, op) in sheet.ops.iter().enumerate() {
            let (when, kind, kind_color) = match op {
                SyncOp::Create(w) => (w.range.start, "New", theme.ok),
                SyncOp::Update { after, .. } => (after.range.start, "Change", theme.focus),
                SyncOp::Move { after, .. } => (after.range.start, "Move", theme.focus),
                SyncOp::Delete(r) => (r.range.start, "Delete", theme.danger),
            };
            let date = local_date(when, &self.ws.tz);
            if last_day != Some(date) {
                last_day = Some(date);
                rows.push(
                    div()
                        .pt_3()
                        .pb_1()
                        .text_xs()
                        .text_color(theme.muted)
                        .child(format!("{} {}", day_name(date), month(date)))
                        .into_any_element(),
                );
            }
            let (issue, detail): (IssueKey, String) = match op {
                SyncOp::Create(w) => (w.issue.clone(), format!("{} ({})", self.fmt_range(&w.range), format_hours(w.range.duration()))),
                SyncOp::Update { before, after } => (
                    after.issue.clone(),
                    if before.range == after.range {
                        "New comment".to_owned()
                    } else {
                        format!(
                            "{} → {} ({} → {})",
                            self.fmt_range(&before.range),
                            self.fmt_range(&after.range),
                            format_hours(before.range.duration()),
                            format_hours(after.range.duration())
                        )
                    },
                ),
                SyncOp::Move { before, after } => (
                    after.issue.clone(),
                    format!("{} from {}, {}", format_hours(after.range.duration()), before.issue, self.fmt_range(&after.range)),
                ),
                SyncOp::Delete(r) => (r.issue.clone(), format!("{} ({})", self.fmt_range(&r.range), format_hours(r.range.duration()))),
            };
            let summary = self.ws.issue(&issue).map(|i| i.summary.clone()).unwrap_or_default();
            let status = sheet.status[index].clone();
            let conflict = match op {
                SyncOp::Update { after, .. } | SyncOp::Move { after, .. } => self.ws.conflicts.contains(&after.id),
                _ => false,
            };
            let note: Option<(SharedString, gpui::Hsla)> = match &status {
                OpStatus::Failed(error) => Some((format!("Not synced: {error}").into(), theme.danger)),
                _ if conflict => Some(("Also changed in Jira since your last sync. Syncing keeps your version.".into(), theme.amber_ink)),
                _ => None,
            };
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.line)
                    .child(
                        div()
                            .w(px(56.))
                            .flex_shrink_0()
                            .text_xs()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(kind_color)
                            .child(kind),
                    )
                    .child(div().w(px(4.)).h(px(28.)).rounded_full().bg(theme.issue(&issue)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .text_sm()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child(SharedString::from(issue.to_string())))
                                    .child(div().truncate().text_color(theme.muted).child(summary)),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted)
                                    .when(matches!(op, SyncOp::Delete(_)), |d| d.line_through())
                                    .child(detail),
                            )
                            .when_some(note, |d, (note, color)| d.child(div().text_xs().text_color(color).child(note))),
                    )
                    .child(div().min_w(px(20.)).flex().justify_end().child(match status {
                        // Before syncing, each change can be dropped on its own.
                        OpStatus::Waiting if !sheet.running && !sheet.finished => div()
                            .id(("undo-change", index))
                            .px_2()
                            .py_0p5()
                            .rounded_md()
                            .text_xs()
                            .text_color(theme.muted)
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.ink).bg(theme.line))
                            .on_click(cx.listener(move |this, _, window, cx| this.undo_change(index, window, cx)))
                            .child("Undo")
                            .into_any_element(),
                        OpStatus::Waiting => div().into_any_element(),
                        OpStatus::Working => div()
                            .size(px(8.))
                            .rounded_full()
                            .bg(theme.focus)
                            .with_animation(
                                ("working", index),
                                Animation::new(millis(700)).repeat().with_easing(gpui::pulsating_between(0.3, 1.)),
                                |d, t| d.opacity(t),
                            )
                            .into_any_element(),
                        OpStatus::Done => div()
                            .text_sm()
                            .text_color(theme.ok)
                            .child("✓")
                            .with_animation(("done", index), Animation::new(millis(220)).with_easing(ease_out_quint()), |d, t| d.opacity(t))
                            .into_any_element(),
                        OpStatus::Failed(_) => div().text_sm().text_color(theme.danger).child("!").into_any_element(),
                    }))
                    .into_any_element(),
            );
        }

        let failed = sheet.status.iter().filter(|s| matches!(s, OpStatus::Failed(_))).count();
        let (title, button): (&str, String) = if sheet.finished && failed > 0 {
            ("Some changes weren't synced", "Close".to_owned())
        } else if sheet.finished {
            ("Synced", "Done".to_owned())
        } else if sheet.running {
            ("Syncing", "Syncing…".to_owned())
        } else {
            let n = sheet.ops.len();
            let button = match sheet.check {
                Check::Running => "Checking Jira…".to_owned(),
                Check::Failed(_) => "Try again".to_owned(),
                Check::Done => format!("Sync {n} {}", if n == 1 { "change" } else { "changes" }),
            };
            ("Review changes", button)
        };
        // Sync waits for the check, and says so instead of looking pressable.
        let waiting = sheet.check == Check::Running && !sheet.finished;
        let check_line: Option<AnyElement> = match &sheet.check {
            _ if sheet.running || sheet.finished => None,
            Check::Running => Some(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted)
                    .child(div().size(px(7.)).rounded_full().bg(theme.focus).with_animation(
                        "checking",
                        Animation::new(millis(700)).repeat().with_easing(gpui::pulsating_between(0.3, 1.)),
                        |d, t| d.opacity(t),
                    ))
                    .child("Checking Jira for changes made there since the last pull…")
                    .into_any_element(),
            ),
            Check::Failed(error) => Some(
                div()
                    .text_xs()
                    .text_color(theme.danger)
                    .child(format!("Couldn't check Jira, so nothing can be synced yet. {error}"))
                    .into_any_element(),
            ),
            Check::Done => None,
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .child(
                div()
                    .id("scrim")
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .bg(theme.scrim)
                    .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.close_sheet(window, cx)))
                    .with_animation("scrim-in", Animation::new(millis(200)), |d, t| d.opacity(t)),
            )
            .child(
                div()
                    .key_context("SyncSheet")
                    .track_focus(&sheet.focus)
                    .occlude()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(SHEET_W))
                    .flex()
                    .flex_col()
                    .bg(theme.surface)
                    .border_l_1()
                    .border_color(theme.line_strong)
                    .shadow_2xl()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .px_5()
                            .pt(px(28.))
                            .pb_3()
                            .border_b_1()
                            .border_color(theme.line)
                            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(title))
                            .child(div().text_sm().text_color(theme.muted).child(format!("{}, {}", parts.join(", "), net_label)))
                            .children(check_line)
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.amber_ink)
                                    .child(if self.store.is_some() { "Nothing is written to Jira until you press Sync." } else { "Syncing only updates this app here. Nothing is sent to Jira." }),
                            ),
                    )
                    .child(div().id("sheet-rows").flex_1().overflow_y_scroll().px_5().pb_4().children(rows))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .px_5()
                            .py_4()
                            .border_t_1()
                            .border_color(theme.line)
                            .child(
                                div()
                                    .id("apply-sync")
                                    .px_4()
                                    .py_2()
                                    .rounded_lg()
                                    .bg(theme.ink)
                                    .text_color(theme.canvas)
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .when(!waiting, |d| d.cursor_pointer())
                                    .when(sheet.running || waiting, |d| d.opacity(0.45))
                                    .on_click(cx.listener(|this, _, window, cx| this.apply_sync(&ApplySync, window, cx)))
                                    .child(button),
                            )
                            .when(!sheet.finished && !sheet.running, |d| {
                                d.child(
                                    div()
                                        .id("cancel-sync")
                                        .px_3()
                                        .py_2()
                                        .rounded_lg()
                                        .text_sm()
                                        .text_color(theme.muted)
                                        .cursor_pointer()
                                        .hover(|s| s.text_color(theme.ink))
                                        .on_click(cx.listener(|this, _, window, cx| this.close_sheet(window, cx)))
                                        .child("Cancel"),
                                )
                            })
                            .child(div().flex_1())
                            .when(!waiting, |d| {
                                let action = match sheet.check {
                                    _ if sheet.finished => "Close",
                                    Check::Failed(_) => "Try again",
                                    _ => "Sync",
                                };
                                d.child(div().text_xs().child(picker::hint(theme, "⌘ Enter", action)))
                            }),
                    )
                    .with_animation(
                        "sheet-in",
                        Animation::new(millis(260)).with_easing(ease_out_quint()),
                        |d, t| d.opacity(t).mr(px(-36. * (1. - t))),
                    ),
            )
    }
}
