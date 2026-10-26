//! The right-click menu on calendar blocks: what can be done to this block, each with
//! its shortcut, so the menu also teaches the keys.

use backfill_core::{IssueKey, Source};
use gpui::{
    Animation, AnimationExt, AnyElement, Context, MouseButton, Pixels, Point, SharedString,
    Window, anchored, deferred, div, ease_out_quint, millis, prelude::*, px, white,
};

use super::{
    Backfill, DeleteSelection, Link, MenuConfirm, MenuNext, MenuPrev, OpenInJira, PickTarget,
    Selected, ToggleAbsorb,
};
use crate::theme::{Theme, with_alpha};

pub(super) const CONTEXT: &str = "ContextMenu";

pub(super) struct Menu {
    pub target: Selected,
    pub position: Point<Pixels>,
    /// The row Enter runs, chosen with the arrow keys or the pointer.
    pub highlighted: Option<usize>,
    /// Counts menus opened, so each one plays its entrance.
    pub generation: usize,
}

#[derive(Clone)]
enum Command {
    Fill(jiff::civil::Date),
    Link,
    LogOn(IssueKey),
    ToggleAbsorb,
    OpenInJira,
    Remove,
}

struct Item {
    label: SharedString,
    key: Option<&'static str>,
    command: Command,
    checked: bool,
    danger: bool,
    /// Starts a group, with a line above it.
    group: bool,
}

fn item(label: impl Into<SharedString>, key: Option<&'static str>, command: Command) -> Item {
    Item { label: label.into(), key, command, checked: false, danger: false, group: false }
}

impl Backfill {
    /// Selects `target` and opens its menu at `position`.
    pub(super) fn open_menu(&mut self, target: Selected, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.close_picker_for_pointer(window, cx);
        self.drag = None;
        self.selected = Some(target);
        if !self.menu_items(target).is_empty() {
            let generation = self.menu.as_ref().map_or(0, |m| m.generation) + 1;
            self.menu = Some(Menu { target, position, highlighted: None, generation });
            window.focus(&self.menu_focus, cx);
        }
        cx.notify();
    }

    pub(super) fn close_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Closes the menu once what it was for is gone, or once something else has focus,
    /// such as the issue search a shortcut opened.
    pub(super) fn validate_menu(&mut self, window: &Window) {
        if let Some(menu) = &self.menu
            && (self.menu_items(menu.target).is_empty() || !self.menu_focus.is_focused(window))
        {
            self.menu = None;
        }
    }

    fn menu_items(&self, target: Selected) -> Vec<Item> {
        match target {
            Selected::Worklog(id) => {
                let Some(worklog) = self.ws.worklog(id) else { return Vec::new() };
                vec![
                    item("Move to another issue…", Some("L"), Command::Link),
                    item(format!("Open {} in Jira", worklog.issue), Some("⌘O"), Command::OpenInJira),
                    Item { danger: true, group: true, ..item("Remove", Some("Delete"), Command::Remove) },
                ]
            }
            Selected::Activity(id) => {
                let Some(activity) = self.ws.activity(id) else { return Vec::new() };
                let hint = self.ws.issue_hint(activity).filter(|k| self.ws.issue(k).is_some());
                let mut items = Vec::new();
                if let Some(key) = &hint {
                    items.push(item(format!("Log on {key}"), None, Command::LogOn(key.clone())));
                }
                let other = if hint.is_some() { "Log on another issue…" } else { "Log on an issue…" };
                items.push(item(other, Some("L"), Command::Link));
                if activity.source == Source::Outlook {
                    let absorb = item("Part of the main task", Some("A"), Command::ToggleAbsorb);
                    items.push(Item { checked: activity.absorbed, group: true, ..absorb });
                }
                if let Some(key) = self.selected_issue_of(target) {
                    items.push(Item { group: true, ..item(format!("Open {key} in Jira"), Some("⌘O"), Command::OpenInJira) });
                }
                items
            }
            Selected::Day(date) => vec![item("Fill the day…", Some("F"), Command::Fill(date))],
        }
    }

    /// The labels in the open menu, ticked ones with a ✓.
    #[cfg(test)]
    pub(super) fn menu_labels(&self) -> Vec<String> {
        let Some(menu) = &self.menu else { return Vec::new() };
        self.menu_items(menu.target).iter().map(|i| format!("{}{}", if i.checked { "✓ " } else { "" }, i.label)).collect()
    }

    fn run_menu_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(menu) = self.menu.take() else { return };
        let Some(item) = self.menu_items(menu.target).into_iter().nth(index) else { return };
        window.focus(&self.focus, cx);
        self.selected = Some(menu.target);
        match item.command {
            Command::Fill(date) => self.fill(date, window, cx),
            Command::Link => self.link(&Link, window, cx),
            Command::LogOn(key) => {
                if let Selected::Activity(id) = menu.target {
                    self.apply_pick(PickTarget::Activity(id), key, cx);
                }
            }
            Command::ToggleAbsorb => self.toggle_absorb(&ToggleAbsorb, window, cx),
            Command::OpenInJira => self.open_in_jira(&OpenInJira, window, cx),
            Command::Remove => self.delete_selection(&DeleteSelection, window, cx),
        }
        cx.notify();
    }

    pub(super) fn menu_next(&mut self, _: &MenuNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_highlight(1, cx);
    }

    pub(super) fn menu_prev(&mut self, _: &MenuPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.move_highlight(-1, cx);
    }

    fn move_highlight(&mut self, step: isize, cx: &mut Context<Self>) {
        let Some(target) = self.menu.as_ref().map(|m| m.target) else { return };
        let count = self.menu_items(target).len() as isize;
        let Some(menu) = self.menu.as_mut() else { return };
        menu.highlighted = Some(match menu.highlighted {
            Some(i) => (i as isize + step).rem_euclid(count) as usize,
            None if step > 0 => 0,
            None => count as usize - 1,
        });
        cx.notify();
    }

    pub(super) fn menu_confirm(&mut self, _: &MenuConfirm, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.menu.as_ref().and_then(|m| m.highlighted) {
            self.run_menu_item(index, window, cx);
        }
    }

    pub(super) fn render_menu(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let mut rows: Vec<AnyElement> = Vec::new();
        for (index, item) in self.menu_items(menu.target).into_iter().enumerate() {
            if item.group && index > 0 {
                rows.push(div().mx_2().my_1().h(px(1.)).bg(theme.line).into_any_element());
            }
            let highlighted = menu.highlighted == Some(index);
            let ink = match (highlighted, item.danger) {
                (true, _) => white(),
                (false, true) => theme.danger,
                (false, false) => theme.ink,
            };
            rows.push(
                div()
                    .id(("menu-item", index))
                    .flex()
                    .items_center()
                    .gap_2()
                    .mx_1()
                    .pl_1p5()
                    .pr_2p5()
                    .py_1()
                    .rounded_md()
                    .text_color(ink)
                    .when(highlighted, |d| d.bg(if item.danger { theme.danger } else { theme.focus }))
                    .cursor_pointer()
                    .on_mouse_move(cx.listener(move |this, _, _, cx| {
                        if let Some(menu) = this.menu.as_mut()
                            && menu.highlighted != Some(index)
                        {
                            menu.highlighted = Some(index);
                            cx.notify();
                        }
                    }))
                    .on_click(cx.listener(move |this, _, window, cx| this.run_menu_item(index, window, cx)))
                    .child(div().w(px(12.)).flex_shrink_0().text_xs().child(if item.checked { "✓" } else { "" }))
                    .child(div().flex_1().whitespace_nowrap().child(item.label))
                    .when_some(item.key, |d, key| {
                        d.child(div().pl_4().text_xs().text_color(if highlighted { with_alpha(white(), 0.75) } else { theme.faint }).child(key))
                    })
                    .into_any_element(),
            );
        }
        let element = div()
            .id("context-menu")
            .key_context(CONTEXT)
            .track_focus(&self.menu_focus)
            .on_action(cx.listener(Self::menu_next))
            .on_action(cx.listener(Self::menu_prev))
            .on_action(cx.listener(Self::menu_confirm))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close_menu(window, cx)))
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .occlude()
            .min_w(px(220.))
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
            .with_animation(("context-menu", menu.generation), Animation::new(millis(110)).with_easing(ease_out_quint()), |d, t| {
                d.opacity(t).mt(px(-3. * (1. - t)))
            });
        Some(deferred(anchored().position(menu.position).snap_to_window_with_margin(px(8.)).child(element)).priority(2).into_any_element())
    }
}
