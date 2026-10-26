//! The week's calendar: day headings with their totals, and the scrolling day columns.
//!
//! Logged time is solid blocks in the issue's colour. Meetings and timers that still need a
//! decision are plain grey blocks with a Log button; once logged time covers one, it's
//! accounted for and no longer drawn. Overlapping blocks sit side by side, like Outlook.

use std::time::{Duration, Instant};

use backfill_core::{
    Activity, ActivityId, IssueKey, Source, TimeRange, Worklog, WorklogId, layout::place,
    time::{format_hours, local_date},
    workspace::SyncState,
};
use gpui::{
    AnyElement, Context, CursorStyle, FontWeight, MouseButton, MouseDownEvent, Pixels, Point,
    ScrollWheelEvent, SharedString, Window, div, prelude::*, px,
};
use jiff::{SignedDuration, Timestamp};

use super::{
    Backfill, DAYHEAD_H, DAYS, Drag, Edge, FOCUS_HOURS, FOCUS_START, GUTTER_W, HEADER_H, Held,
    LANE_H, LONG_EVENT, LONG_ROW_H, Layout, MAX_LANES, PickTarget, SCROLL_SPRING, Selected, gauge,
    ratio, weekday,
};
use crate::{
    motion::{BlockKey, Entrance, Geom, Spot},
    outlook,
    theme::{Theme, with_alpha},
};

/// Drags land on the quarter hour.
const SNAP: f32 = 15.;

/// An event under the day headings, from its first day column to its last.
pub(super) struct Banner {
    pub activity: Activity,
    pub first: usize,
    pub last: usize,
    pub lane: usize,
}

/// Something drawn in a day column.
enum Item {
    Logged(Worklog),
    /// A meeting or timer that isn't logged.
    Unlogged(Activity),
    /// Deleted here, still in Jira until the next sync.
    Removed { issue: IssueKey, key: u64 },
    /// Time waiting for an issue, by its place among the drafts. While drawing, the
    /// block under the pointer is the next one.
    Draft(usize),
}

impl Backfill {
    /// The y of `minutes` past midnight, relative to the top of the calendar, as scrolled.
    pub(super) fn calendar_y(&self, l: &Layout, minutes: f32) -> f32 {
        (minutes / 60. - self.scroll_shown) * l.hour_h
    }

    /// Minutes past midnight at a window y.
    fn minutes_at(&self, l: &Layout, window_y: f32) -> f32 {
        ((window_y - l.cal_top) / l.hour_h + self.scroll_shown) * 60.
    }

    fn max_scroll(l: &Layout) -> f32 {
        (24. - l.visible_hours()).max(0.)
    }

    /// Scrolls so `minutes` is comfortably in view, if it isn't already.
    pub(super) fn scroll_into_view(&mut self, minutes: f32) {
        let Some(l) = self.layout else { return };
        let hour = minutes / 60.;
        let top = self.scroll_shown;
        if hour >= top + 0.25 && hour <= top + l.visible_hours() - 1. {
            return;
        }
        let target = (hour - 1.).clamp(0., Self::max_scroll(&l));
        self.scroll.set(target, SCROLL_SPRING, Instant::now());
    }

    fn on_scroll(&mut self, event: &ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(l) = self.layout else { return };
        let dy = f32::from(event.delta.pixel_delta(px(l.hour_h / 2.)).y);
        let top = (self.scroll_shown - dy / l.hour_h).clamp(0., Self::max_scroll(&l));
        self.scroll.jump_to(top);
        self.scroll_shown = top;
        cx.notify();
    }

    pub(super) fn close_picker_for_pointer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.picker.is_some() {
            self.close_picker(window, cx);
        }
    }

    fn on_calendar_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(l) = self.layout else { return };
        let Some(day) = l.day_at(f32::from(event.position.x)) else { return };
        self.close_picker_for_pointer(window, cx);
        let minutes = snap(self.minutes_at(&l, f32::from(event.position.y)));
        self.drag = Some(Drag::Draw { day, anchor: minutes, current: minutes, on: None });
        cx.notify();
    }

    fn on_worklog_mouse_down(&mut self, id: WorklogId, edge: Option<Edge>, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.modifiers.control {
            return self.open_menu(Selected::Worklog(id), event.position, window, cx);
        }
        cx.stop_propagation();
        self.close_picker_for_pointer(window, cx);
        self.selected = Some(Selected::Worklog(id));
        if event.click_count >= 2 {
            self.drag = None;
            self.open_picker(PickTarget::Worklog(id), window, cx);
            return;
        }
        let (Some(l), Some(worklog)) = (self.layout, self.ws.worklog(id)) else { return };
        let Some(day) = self.day_index(backfill_core::time::local_date(worklog.range.start, &self.ws.tz)) else { return };
        let start = self.minutes(worklog.range.start, day);
        let end = self.minutes(worklog.range.end, day);
        self.drag = Some(match edge {
            Some(edge) => Drag::Resize { held: Held::Worklog(id), day, start, end, edge },
            None => {
                let pointer = self.minutes_at(&l, f32::from(event.position.y));
                Drag::Move { held: Held::Worklog(id), day, start, duration: end - start, grab: pointer - start, origin: event.position, moved: false }
            }
        });
        cx.notify();
    }

    /// A press on a draft moves it, or resizes it from an edge. The picker stays open.
    fn on_draft_mouse_down(&mut self, index: usize, edge: Option<Edge>, event: &MouseDownEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        let (Some(l), Some(range)) = (self.layout, self.drafts.get(index).copied()) else { return };
        let Some(day) = self.day_index(backfill_core::time::local_date(range.start, &self.ws.tz)) else { return };
        let (start, end) = (self.minutes(range.start, day), self.minutes(range.end, day));
        self.drag = Some(match edge {
            Some(edge) => Drag::Resize { held: Held::Draft(index), day, start, end, edge },
            None => {
                let pointer = self.minutes_at(&l, f32::from(event.position.y));
                Drag::Move { held: Held::Draft(index), day, start, duration: end - start, grab: pointer - start, origin: event.position, moved: false }
            }
        });
        cx.notify();
    }

    /// Whether `position` is on a draft, as drawn last frame.
    pub(super) fn draft_at(&self, position: Point<Pixels>) -> bool {
        let (x, y) = (f32::from(position.x), f32::from(position.y));
        self.rendered
            .iter()
            .any(|(key, g)| matches!(key, BlockKey::Draft(_)) && x >= g.x && x <= g.x + g.w && y >= g.y && y <= g.y + g.h)
    }

    /// A press on a meeting selects it, and also starts drawing: dragging from a meeting
    /// logs time like dragging anywhere else, so meetings never get in the way.
    fn on_unlogged_mouse_down(&mut self, id: ActivityId, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.modifiers.control {
            return self.open_menu(Selected::Activity(id), event.position, window, cx);
        }
        cx.stop_propagation();
        self.close_picker_for_pointer(window, cx);
        self.selected = Some(Selected::Activity(id));
        if event.click_count >= 2 {
            self.open_picker(PickTarget::Activity(id), window, cx);
        } else if let Some(l) = self.layout
            && let Some(day) = l.day_at(f32::from(event.position.x))
        {
            let minutes = snap(self.minutes_at(&l, f32::from(event.position.y)));
            self.drag = Some(Drag::Draw { day, anchor: minutes, current: minutes, on: Some(id) });
        }
        cx.notify();
    }

    /// Whether logged time already covers all of `activity`. Then it's accounted for and
    /// isn't drawn.
    fn covered(&self, activity: &Activity) -> bool {
        let date = backfill_core::time::local_date(activity.range.start, &self.ws.tz);
        let logged: SignedDuration = self
            .ws
            .worklogs_on(date)
            .filter_map(|w| w.range.clip(&activity.range))
            .map(|r| r.duration())
            .sum();
        logged + SignedDuration::from_mins(1) >= activity.range.duration()
    }

    /// Meetings and timers on `date` that still need a decision.
    fn unlogged_on(&self, date: jiff::civil::Date) -> Vec<Activity> {
        self.ws
            .activities_on(date)
            .filter(|a| matches!(a.source, Source::Outlook | Source::Toggl))
            .filter(|a| self.ws.worklog_for(a.id).is_none() && !self.covered(a))
            .cloned()
            .collect()
    }

    /// The part of `activity` on `date`: a meeting over several days has its hours there.
    fn part_on(&self, activity: &Activity, date: jiff::civil::Date) -> TimeRange {
        let day = self.ws.day(date);
        self.ws.day_parts(activity).into_iter().find(|p| p.overlaps(&day)).unwrap_or(activity.range)
    }

    /// Whether `activity` goes under the headings on `date` instead of in the grid: it's
    /// over several days, or long.
    fn in_banner_row(&self, activity: &Activity, date: jiff::civil::Date) -> bool {
        let tz = &self.ws.tz;
        let several_days = local_date(activity.range.start, tz) != local_date(activity.range.end - SignedDuration::from_nanos(1), tz);
        several_days || self.part_on(activity, date).duration() >= LONG_EVENT
    }

    /// Unlogged events on `date` that go under the headings instead of in the grid.
    pub(super) fn long_events(&self, date: jiff::civil::Date) -> Vec<Activity> {
        self.unlogged_on(date).into_iter().filter(|a| self.in_banner_row(a, date)).collect()
    }

    /// The week's long events and ones over several days, each spanning the day columns
    /// it touches. Overlapping ones get lanes of their own, up to a few.
    pub(super) fn banners(&self) -> Vec<Banner> {
        let mut banners: Vec<Banner> = Vec::new();
        for day in 0..DAYS {
            for activity in self.long_events(self.date(day)) {
                match banners.iter_mut().find(|b| b.activity.id == activity.id) {
                    Some(banner) => banner.last = day,
                    None => banners.push(Banner { activity, first: day, last: day, lane: 0 }),
                }
            }
        }
        banners.sort_by_key(|b| (b.first, std::cmp::Reverse(b.last)));
        let mut lane_ends: Vec<usize> = Vec::new();
        banners.retain_mut(|banner| {
            let lane = lane_ends.iter().position(|end| *end < banner.first).unwrap_or(lane_ends.len());
            if lane >= MAX_LANES {
                return false;
            }
            match lane_ends.get_mut(lane) {
                Some(end) => *end = banner.last,
                None => lane_ends.push(banner.last),
            }
            banner.lane = lane;
            true
        });
        banners
    }

    /// The Log button on a meeting: one click when the title names an issue, otherwise
    /// the issue search.
    fn log_activity(&mut self, id: ActivityId, window: &mut Window, cx: &mut Context<Self>) {
        let hint = self.ws.activity(id).and_then(|a| self.ws.issue_hint(a));
        match hint.filter(|key| self.ws.issue(key).is_some()) {
            Some(key) => self.apply_pick(PickTarget::Activity(id), key, cx),
            None => {
                self.selected = Some(Selected::Activity(id));
                self.open_picker(PickTarget::Activity(id), window, cx);
            }
        }
    }

    pub(super) fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(l) = self.layout else { return };
        let pointer = self.minutes_at(&l, f32::from(position.y));
        let day_under = l.day_at(f32::from(position.x));
        match self.drag.as_mut() {
            Some(Drag::Draw { current, .. }) => *current = snap(pointer).clamp(0., 24. * 60.),
            Some(Drag::Move { day, start, duration, grab, origin, moved, .. }) => {
                let distance = (position.y - origin.y).abs() + (position.x - origin.x).abs();
                if f32::from(distance) > 4. {
                    *moved = true;
                }
                if *moved {
                    *start = snap(pointer - *grab).clamp(0., 24. * 60. - *duration);
                    if let Some(d) = day_under {
                        *day = d;
                    }
                }
            }
            Some(Drag::Resize { start, end, edge, .. }) => match edge {
                Edge::Bottom => *end = snap(pointer).max(*start + SNAP).min(24. * 60.),
                Edge::Top => *start = snap(pointer).min(*end - SNAP).max(0.),
            },
            None => return,
        }
        cx.notify();
    }

    pub(super) fn finish_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        match drag {
            Drag::Draw { day, anchor, current, on } => {
                let (from, to) = (anchor.min(current), anchor.max(current));
                if to - from >= self.ws.settings.min_block.as_mins() as f32 {
                    self.drafts = vec![TimeRange::new(self.at_minutes(from, day), self.at_minutes(to, day))];
                    self.open_picker(PickTarget::Drafts { fill: None }, window, cx);
                } else {
                    self.selected = on.map(Selected::Activity);
                }
            }
            Drag::Move { held, day, start, duration, moved, .. } => {
                if moved {
                    let range = TimeRange::new(self.at_minutes(start, day), self.at_minutes(start + duration, day));
                    self.set_held_range(held, range, window, cx);
                }
            }
            Drag::Resize { held, day, start, end, .. } => {
                let range = TimeRange::new(self.at_minutes(start, day), self.at_minutes(end, day));
                self.set_held_range(held, range, window, cx);
            }
        }
        cx.notify();
    }

    /// Puts a moved or resized block at `range`. A worklog's change can be undone. A
    /// draft's updates the picker, which follows it.
    fn set_held_range(&mut self, held: Held, range: TimeRange, window: &mut Window, cx: &mut Context<Self>) {
        match held {
            Held::Worklog(id) => {
                if self.ws.worklog(id).is_some_and(|w| w.range != range) {
                    self.checkpoint();
                    self.ws.set_range(id, range);
                }
            }
            Held::Draft(index) => {
                let Some(draft) = self.drafts.get_mut(index) else { return };
                *draft = range;
                let Some(picker) = &self.picker else { return };
                let PickTarget::Drafts { fill } = picker.target else { return };
                let (title, anchor) = (self.drafts_title(fill), self.anchor_for(&picker.target, window));
                let entity = picker.entity.clone();
                entity.update(cx, |picker, cx| picker.set_title(title, cx));
                if let Some(picker) = self.picker.as_mut() {
                    picker.anchor = anchor;
                }
            }
        }
    }

    /// The day and range a block is drawn at: its own, or the drag's while it moves.
    fn shown(&self, held: Held, day: usize, range: TimeRange) -> (usize, TimeRange) {
        match &self.drag {
            Some(Drag::Move { held: h, day: d, start, duration, moved: true, .. }) if *h == held => {
                (*d, TimeRange::new(self.at_minutes(*start, *d), self.at_minutes(*start + *duration, *d)))
            }
            Some(Drag::Resize { held: h, start, end, .. }) if *h == held => {
                (day, TimeRange::new(self.at_minutes(*start, day), self.at_minutes(*end, day)))
            }
            _ => (day, range),
        }
    }

    pub(super) fn render_day_heads(&mut self, theme: &Theme, l: Layout, cx: &mut Context<Self>) -> AnyElement {
        let target = self.ws.settings.target;
        let mut heads: Vec<AnyElement> = Vec::new();
        for day in 0..DAYS {
            let date = self.date(day);
            let logged = self.ws.logged_on(date);
            let is_today = date == self.today;
            let happened = self.has_happened(date);
            let complete = logged >= target;
            let selected = self.selected == Some(Selected::Day(date));
            let color = if complete { theme.ink } else if happened { theme.amber } else { theme.faint };
            heads.push(
                div()
                    .id(("day", day))
                    .absolute()
                    .left(px(l.day_x(day)))
                    .top(px(HEADER_H + 8.))
                    .w(px(l.col_w))
                    .h(px(DAYHEAD_H - 14.))
                    .flex()
                    .flex_col()
                    .justify_center()
                    .gap_1p5()
                    .px_2()
                    .rounded_lg()
                    .cursor_pointer()
                    .when(selected, |d| d.bg(theme.raised))
                    .hover(|s| s.bg(with_alpha(theme.raised, 0.6)))
                    // A double click fills the day.
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        this.selected = Some(Selected::Day(date));
                        if event.click_count() >= 2 {
                            this.fill(date, window, cx);
                        }
                        cx.notify();
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.open_menu(Selected::Day(date), event.position, window, cx)
                    }))
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_1p5()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(if is_today { theme.amber_ink } else { theme.ink })
                                    .child(weekday(date)),
                            )
                            .child(div().text_sm().text_color(theme.muted).child(date.day().to_string()))
                            .when(is_today, |d| d.child(div().text_xs().text_color(theme.amber_ink).child("today")))
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(if complete || !happened { theme.muted } else { theme.amber_ink })
                                    .child(format!("{} / {}", format_hours(logged), format_hours(target))),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(px(20.))
                            .child(div().flex_1().child(gauge(theme, ("day-gauge", day), ratio(logged, target), 0., color)))
                    )
                    .into_any_element(),
            );
        }
        // Long events and ones over several days, under the headings, so they never cover
        // the day. One over several days is one bar across them, like Outlook's.
        for (index, banner) in self.banners().into_iter().enumerate() {
            let activity = banner.activity;
            let id = activity.id;
            let selected = self.selected == Some(Selected::Activity(id));
            let tz = &self.ws.tz;
            let starts_here = local_date(activity.range.start, tz) == self.date(banner.first);
            let ends_here = local_date(activity.range.end - SignedDuration::from_nanos(1), tz) == self.date(banner.last);
            let left = l.day_x(banner.first);
            heads.push(
                div()
                    .id(("banner", index))
                    .absolute()
                    .left(px(left))
                    .top(px(HEADER_H + DAYHEAD_H - 6. + banner.lane as f32 * LANE_H))
                    .w(px(l.day_x(banner.last) + l.col_w - left))
                    .h(px(LONG_ROW_H - 6.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .rounded_md()
                    .bg(theme.raised)
                    .border_1()
                    .border_color(if selected { theme.focus } else { theme.line_strong })
                    .text_xs()
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        this.open_menu(Selected::Activity(id), event.position, window, cx)
                    }))
                    .on_click(cx.listener(move |this, event: &gpui::ClickEvent, window, cx| {
                        this.selected = Some(Selected::Activity(id));
                        if event.click_count() >= 2 {
                            this.open_picker(PickTarget::Activity(id), window, cx);
                        }
                        cx.notify();
                    }))
                    .when(starts_here, |d| d.child(div().flex_shrink_0().text_color(theme.muted).child(self.fmt_time(activity.range.start))))
                    .child(div().flex_1().min_w_0().truncate().child(SharedString::from(activity.title.clone())))
                    .when(ends_here, |d| d.child(div().flex_shrink_0().text_color(theme.muted).child(self.fmt_time(activity.range.end))))
                    .into_any_element(),
            );
        }
        div().children(heads).into_any_element()
    }

    pub(super) fn render_calendar(&mut self, theme: &Theme, l: Layout, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let now = Instant::now();
        let scroll = self.scroll.sample(now);
        if scroll.is_active {
            window.request_animation_frame();
        }
        self.scroll_shown = scroll.value.clamp(0., Self::max_scroll(&l));
        let scroll_px = self.scroll_shown * l.hour_h;
        self.rendered.clear();

        let mut back: Vec<AnyElement> = Vec::new();
        let mut front: Vec<AnyElement> = Vec::new();

        // Hours down the left.
        for hour in 0..=24 {
            let y = self.calendar_y(&l, hour as f32 * 60.);
            if y < -8. || y > l.cal_h + 8. {
                continue;
            }
            back.push(
                div()
                    .absolute()
                    .left_0()
                    .top(px(y - 7.))
                    .w(px(GUTTER_W - 12.))
                    .text_right()
                    .text_xs()
                    .text_color(theme.faint)
                    .child(format!("{hour:02}:00"))
                    .into_any_element(),
            );
        }

        // Every worklog in the week, at the day it's drawn on (a drag can carry it across).
        let mut items: Vec<Vec<(TimeRange, Item)>> = (0..DAYS).map(|_| Vec::new()).collect();
        for day in 0..DAYS {
            let date = self.date(day);
            for worklog in self.ws.worklogs_on(date) {
                if backfill_core::time::local_date(worklog.range.start, &self.ws.tz) != date {
                    continue;
                }
                let (shown_day, range) = self.shown(Held::Worklog(worklog.id), day, worklog.range);
                items[shown_day].push((range, Item::Logged(worklog.clone())));
            }
            for activity in self.unlogged_on(date) {
                if self.in_banner_row(&activity, date) {
                    continue;
                }
                let range = self.part_on(&activity, date);
                let range = range.clip(&self.ws.day(date)).unwrap_or(range);
                items[day].push((range, Item::Unlogged(activity)));
            }
            for removed in self.ws.deleted_on(date) {
                let key = removed.id.0.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
                items[day].push((removed.range, Item::Removed { issue: removed.issue.clone(), key }));
            }
        }
        // Drafts take their place among the rest, so overlapping blocks make room for them
        // while they're drawn and dragged, not only once they're logged.
        for (index, draft) in self.drafts.iter().enumerate() {
            let Some(day) = self.day_index(backfill_core::time::local_date(draft.start, &self.ws.tz)) else { continue };
            let (day, range) = self.shown(Held::Draft(index), day, *draft);
            items[day].push((range, Item::Draft(index)));
        }
        if let Some(Drag::Draw { day, anchor, current, .. }) = &self.drag
            && current != anchor
        {
            let range = TimeRange::new(self.at_minutes(anchor.min(*current), *day), self.at_minutes(anchor.max(*current), *day));
            items[*day].push((range, Item::Draft(self.drafts.len())));
        }

        for (day, day_items) in items.into_iter().enumerate() {
            let x = l.day_x(day);
            back.push(self.render_column(theme, &l, x));

            self.render_jira_copies(theme, &l, day, &mut front);
            let stagger = if self.entering_week { Duration::from_millis(day as u64 * 45) } else { Duration::ZERO };
            let ranges: Vec<TimeRange> = day_items.iter().map(|(r, _)| *r).collect();
            for ((range, item), placement) in day_items.iter().zip(place(&ranges)) {
                let (left, width) = placement.fractions();
                let target = Spot::new(day, left, width, self.minutes(range.start, day), self.minutes(range.end, day));
                let (key, entrance) = match item {
                    Item::Logged(w) => (BlockKey::Worklog(w.id.0), Entrance::Pour),
                    Item::Unlogged(a) => (BlockKey::Activity(a.id.0), Entrance::Fade),
                    Item::Removed { key, .. } => (BlockKey::Removed(*key), Entrance::Fade),
                    Item::Draft(index) => (BlockKey::Draft(*index), Entrance::Instant),
                };
                let g = l.block(self.blocks.place(key, target, entrance, stagger, now), scroll_px);
                self.rendered.push((key, Geom { y: g.y + l.cal_top, ..g }));
                front.push(match item {
                    Item::Logged(w) => self.render_logged(theme, w, *range, g, cx),
                    Item::Unlogged(a) => self.render_unlogged(theme, a, *range, g, cx),
                    Item::Removed { issue, .. } => render_removed(theme, issue, *range, g),
                    Item::Draft(index) => self.render_draft(theme, *index, *range, g, cx),
                });
            }

            // Pills for anything scrolled out of view.
            let top = self.scroll_shown * 60.;
            let bottom = top + l.visible_hours() * 60.;
            let ends: Vec<f32> = ranges.iter().map(|r| self.minutes(r.end, day)).filter(|end| *end <= top).collect();
            let starts: Vec<f32> = ranges.iter().map(|r| self.minutes(r.start, day)).filter(|start| *start >= bottom).collect();
            if let Some(&last) = ends.iter().max_by(|a, b| a.total_cmp(b)) {
                front.push(edge_pill(theme, ("earlier", day), x, l.col_w, 6., format!("↑ {} earlier", ends.len()), last - 60., cx));
            }
            if let Some(&first) = starts.iter().min_by(|a, b| a.total_cmp(b)) {
                front.push(edge_pill(theme, ("later", day), x, l.col_w, l.cal_h - 28., format!("↓ {} later", starts.len()), first, cx));
            }
        }

        // Now.
        if let Some(day) = self.day_index(self.today) {
            let y = self.calendar_y(&l, self.minutes(Timestamp::now(), day));
            if (0. ..=l.cal_h).contains(&y) {
                front.push(
                    div()
                        .absolute()
                        .left(px(l.day_x(day) - 3.))
                        .top(px(y - 3.))
                        .w(px(l.col_w + 3.))
                        .h(px(6.))
                        .flex()
                        .items_center()
                        .child(div().size(px(6.)).rounded_full().bg(theme.amber))
                        .child(div().flex_1().h(px(2.)).bg(theme.amber))
                        .into_any_element(),
                );
            }
        }

        if let Some(notice) = self.render_calendar_notice(theme, l, cx) {
            front.push(notice);
        }

        div()
            .id("calendar")
            .absolute()
            .left_0()
            .top(px(l.cal_top))
            .w(px(l.width))
            .h(px(l.cal_h))
            .overflow_hidden()
            .cursor(if matches!(self.drag, Some(Drag::Draw { .. })) { CursorStyle::Crosshair } else { CursorStyle::Arrow })
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_calendar_mouse_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll))
            .children(back)
            .children(front)
            .into_any_element()
    }

    /// A day column, with hour lines and the hours outside the usual workday shaded.
    fn render_column(&self, theme: &Theme, l: &Layout, x: f32) -> AnyElement {
        let shade = |from: f32, to: f32| {
            let (y0, y1) = (self.calendar_y(l, from * 60.), self.calendar_y(l, to * 60.));
            div().absolute().left_0().right_0().top(px(y0)).h(px(y1 - y0)).bg(with_alpha(theme.canvas, 0.45))
        };
        let lines = (1..24).map(|hour| {
            div().absolute().left_0().right_0().top(px(self.calendar_y(l, hour as f32 * 60.))).h(px(1.)).bg(with_alpha(theme.line, 0.8))
        });
        div()
            .absolute()
            .left(px(x))
            .top_0()
            .w(px(l.col_w))
            .h(px(l.cal_h))
            .rounded_lg()
            .bg(theme.surface)
            .border_1()
            .border_color(theme.line)
            .overflow_hidden()
            .children(lines)
            .child(shade(0., FOCUS_START))
            .child(shade(FOCUS_START + FOCUS_HOURS, 24.))
            .into_any_element()
    }

    /// Logged time: solid, in the issue's colour. A dot means Jira doesn't have it yet.
    fn render_logged(&self, theme: &Theme, worklog: &Worklog, range: TimeRange, g: Geom, cx: &mut Context<Self>) -> AnyElement {
        let id = worklog.id;
        let selected = self.selected == Some(Selected::Worklog(id));
        let state = self.ws.sync_state(worklog);
        let color = theme.issue(&worklog.issue);
        // New time is drawn lighter, behind a dashed edge, until Jira has it.
        let ink = if state == SyncState::New { theme.ink } else { theme.issue_ink(&worklog.issue) };
        let border = match state {
            _ if selected => theme.ink,
            SyncState::New => color,
            SyncState::Changed => theme.amber,
            SyncState::Synced => with_alpha(theme.surface, 0.6),
        };
        let badge = self.difference(worklog).map(|(label, new)| {
            let tint = if new { theme.ok } else { theme.amber_ink };
            div()
                .flex_shrink_0()
                .px_1()
                .rounded_sm()
                .bg(theme.surface)
                .border_1()
                .border_color(tint)
                .text_size(px(10.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(tint)
                .child(label)
        });
        let summary: SharedString = self.ws.issue(&worklog.issue).map(|i| i.summary.clone()).unwrap_or_default().into();
        // What it was logged from says more than the issue's summary, unless it only
        // repeats the key, as a Toggl entry named "APP-123" does.
        let meeting = worklog.origin.and_then(|a| self.ws.activity(a)).and_then(|a| title_without_keys(&a.title));
        let label: SharedString = match meeting {
            Some(title) => title.into(),
            None if summary.is_empty() => SharedString::default(),
            None => summary.clone(),
        };
        let dragging = matches!(&self.drag, Some(Drag::Move { held, moved: true, .. } | Drag::Resize { held, .. }) if *held == Held::Worklog(id));
        let edge = |name: &'static str, edge: Edge| {
            div()
                .id((name, id.0))
                .absolute()
                .left_0()
                .right_0()
                .h(px(6.))
                .cursor(CursorStyle::ResizeUpDown)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, event, window, cx| this.on_worklog_mouse_down(id, Some(edge), event, window, cx)))
        };
        div()
            .id(("worklog", id.0))
            .absolute()
            .left(px(g.x))
            .top(px(g.y))
            .w(px(g.w))
            .h(px(g.h))
            .opacity(g.opacity)
            .rounded_md()
            .overflow_hidden()
            .bg(if state == SyncState::New { with_alpha(color, 0.4) } else { color })
            .text_color(ink)
            .border_2()
            .when(state == SyncState::New, |d| d.border_dashed())
            .border_color(border)
            .when(dragging, |d| d.shadow_lg())
            .cursor(if dragging { CursorStyle::ClosedHand } else { CursorStyle::PointingHand })
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event, window, cx| this.on_worklog_mouse_down(id, None, event, window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.open_menu(Selected::Worklog(id), event.position, window, cx)
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px_1p5()
                    .when(g.h < 26., |d| d.h_full().justify_center().text_size(px(11.)))
                    .when(g.h >= 26., |d| d.py_0p5().text_xs())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(div().font_weight(FontWeight::SEMIBOLD).flex_shrink_0().child(SharedString::from(worklog.issue.to_string())))
                            .child(div().flex_1().min_w_0().truncate().opacity(0.8).child(label))
                            .child(div().flex_shrink_0().child(format_hours(range.duration())))
                            .children(badge),
                    )
                    .when(g.h > 44., |d| {
                        d.child(div().opacity(0.8).truncate().child(self.fmt_range(&range)))
                    }),
            )
            .child(edge("top-edge", Edge::Top).top_0())
            .child(edge("bottom-edge", Edge::Bottom).bottom_0())
            .into_any_element()
    }

    /// How a worklog differs from Jira, for its badge: "New", or what changed, such as
    /// "+30m", "1h later" or "from APP-1". The flag is set for new ones.
    pub(super) fn difference(&self, worklog: &Worklog) -> Option<(String, bool)> {
        match self.ws.sync_state(worklog) {
            SyncState::Synced => None,
            SyncState::New => Some(("New".into(), true)),
            SyncState::Changed => {
                let before = self.ws.jira_copy(worklog)?;
                let signed = |d: SignedDuration| format!("{}{}", if d.is_negative() { "−" } else { "+" }, format_hours(d.abs()));
                let mut parts = Vec::new();
                if before.issue != worklog.issue {
                    parts.push(format!("from {}", before.issue));
                }
                let longer = worklog.range.duration() - before.range.duration();
                let later = worklog.range.start.duration_since(before.range.start);
                if !longer.is_zero() {
                    parts.push(signed(longer));
                } else if later.is_positive() {
                    parts.push(format!("{} later", format_hours(later)));
                } else if later.is_negative() {
                    parts.push(format!("{} earlier", format_hours(later.abs())));
                }
                if parts.is_empty() {
                    parts.push("new comment".into());
                }
                Some((parts.join(", "), false))
            }
        }
    }

    /// Where Jira still has a changed worklog's time: a faint dashed outline behind the
    /// blocks, so a move or resize shows how far it went.
    fn render_jira_copies(&self, theme: &Theme, l: &Layout, day: usize, front: &mut Vec<AnyElement>) {
        let date = self.date(day);
        for worklog in &self.ws.worklogs {
            let Some(before) = self.ws.jira_copy(worklog) else { continue };
            if before.range == worklog.range || backfill_core::time::local_date(before.range.start, &self.ws.tz) != date {
                continue;
            }
            let (y0, y1) = (self.calendar_y(l, self.minutes(before.range.start, day)), self.calendar_y(l, self.minutes(before.range.end, day)));
            front.push(
                div()
                    .absolute()
                    .left(px(l.day_x(day) + 2.))
                    .top(px(y0))
                    .w(px(l.col_w - 4.))
                    .h(px((y1 - y0 - 1.).max(14.)))
                    .rounded_md()
                    .border_1()
                    .border_dashed()
                    .border_color(with_alpha(theme.amber, 0.7))
                    .bg(with_alpha(theme.amber, 0.05))
                    .into_any_element(),
            );
        }
    }

    /// Time waiting for an issue. Its body moves it and its edges resize it, while the
    /// picker stays open for choosing the issue.
    fn render_draft(&self, theme: &Theme, index: usize, range: TimeRange, g: Geom, cx: &mut Context<Self>) -> AnyElement {
        let held = Held::Draft(index);
        let dragging = matches!(&self.drag, Some(Drag::Move { held: h, moved: true, .. } | Drag::Resize { held: h, .. }) if *h == held)
            || matches!(&self.drag, Some(Drag::Draw { .. }));
        let edge = |name: &'static str, edge: Edge| {
            div()
                .id((name, index))
                .absolute()
                .left_0()
                .right_0()
                .h(px(6.))
                .cursor(CursorStyle::ResizeUpDown)
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, event, _, cx| this.on_draft_mouse_down(index, Some(edge), event, cx)))
        };
        div()
            .id(("draft", index))
            .absolute()
            .left(px(g.x))
            .top(px(g.y))
            .w(px(g.w))
            .h(px(g.h))
            .rounded_md()
            .overflow_hidden()
            .bg(with_alpha(theme.focus, 0.18))
            .border_2()
            .border_color(theme.focus)
            .when(dragging, |d| d.shadow_lg())
            .cursor(if dragging { CursorStyle::ClosedHand } else { CursorStyle::OpenHand })
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event, _, cx| this.on_draft_mouse_down(index, None, event, cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px_1p5()
                    .text_xs()
                    .when(g.h < 26., |d| d.h_full().justify_center().text_size(px(11.)))
                    .when(g.h >= 26., |d| d.py_0p5())
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(format_hours(range.duration())))
                    .when(g.h > 40., |d| d.child(div().text_color(theme.muted).truncate().child(self.fmt_range(&range)))),
            )
            .child(edge("draft-top", Edge::Top).top_0())
            .child(edge("draft-bottom", Edge::Bottom).bottom_0())
            .into_any_element()
    }

    /// A meeting or timer that still needs a decision: a plain grey block with a Log button
    /// while hovered or selected. A meeting that names an issue has a stripe in its colour.
    ///
    /// The button shows through a hover style on the block, not through state. It sits
    /// inside the block, so the block stays hovered while the pointer is on the button.
    fn render_unlogged(&self, theme: &Theme, activity: &Activity, range: TimeRange, g: Geom, cx: &mut Context<Self>) -> AnyElement {
        let id = activity.id;
        let selected = self.selected == Some(Selected::Activity(id));
        let hint = self.ws.issue_hint(activity).filter(|k| self.ws.issue(k).is_some());
        let stripe = hint.as_ref().map_or(theme.faint, |k| theme.issue(k));
        let group: SharedString = format!("unlogged-{}", id.0).into();
        let meta: SharedString = match (activity.absorbed, activity.source) {
            (true, _) => "Part of the main task".into(),
            (false, Source::Toggl) => format!("Toggl, {}", self.fmt_range(&range)).into(),
            (false, _) => self.fmt_range(&range).into(),
        };
        let log_label: SharedString = match &hint {
            Some(key) => format!("Log on {key}").into(),
            None => "Log…".into(),
        };
        div()
            .id(("unlogged", id.0))
            .group(group.clone())
            .absolute()
            .left(px(g.x))
            .top(px(g.y))
            .w(px(g.w))
            .h(px(g.h))
            .opacity(if activity.absorbed { 0.6 } else { g.opacity })
            .flex()
            .rounded_md()
            .overflow_hidden()
            .bg(theme.raised)
            .border_1()
            .border_color(if selected { theme.focus } else { theme.line_strong })
            .when(selected, |d| d.border_2())
            .cursor_pointer()
            .child(div().w(px(3.)).h_full().flex_shrink_0().bg(stripe))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, event, window, cx| this.on_unlogged_mouse_down(id, event, window, cx)))
            .on_mouse_down(MouseButton::Right, cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                this.open_menu(Selected::Activity(id), event.position, window, cx)
            }))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .px_1p5()
                    .when(g.h < 26., |d| d.h_full().justify_center().text_size(px(11.)))
                    .when(g.h >= 26., |d| d.py_0p5().text_xs())
                    .child(
                        div()
                            .text_color(if activity.absorbed { theme.muted } else { theme.ink })
                            .when(g.h < 40., |d| d.truncate())
                            .when(g.h >= 40., |d| d.line_clamp(2))
                            .child(SharedString::from(activity.title.clone())),
                    )
                    .when(g.h > 44., |d| d.child(div().text_color(theme.muted).truncate().child(meta))),
            )
            .child(
                div()
                    .id(("log", id.0))
                    .absolute()
                    .top(px(if g.h < 26. { 0. } else { 3. }))
                    .right(px(3.))
                    .px_1p5()
                    .rounded_sm()
                    .bg(hint.as_ref().map_or(theme.ink, |k| theme.issue(k)))
                    .text_color(hint.as_ref().map_or(theme.canvas, |k| theme.issue_ink(k)))
                    .text_xs()
                    .font_weight(FontWeight::MEDIUM)
                    .cursor_pointer()
                    .when(!selected, |d| d.opacity(0.).group_hover(group, |s| s.opacity(1.)))
                    .on_click(cx.listener(move |this, _, window, cx| this.log_activity(id, window, cx)))
                    .child(log_label),
            )
            .into_any_element()
    }

    /// Says what to do when Outlook meetings can't be read, across the top of the calendar.
    fn render_calendar_notice(&self, theme: &Theme, l: Layout, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (body, settings): (&str, bool) = match self.outlook {
            outlook::Status::NotBundled => (
                "macOS only lets apps read the calendar, and this copy of backfill was started as a plain program. Start it with cargo run, which opens it as an app.",
                false,
            ),
            outlook::Status::Denied => ("backfill isn't allowed to read your calendar. Allow it under Calendars in Privacy & Security, then come back.", true),
            _ => return None,
        };
        Some(
            div()
                .absolute()
                .left(px(l.day_x(0) + 8.))
                .top(px(8.))
                .w(px(l.day_x(DAYS - 1) + l.col_w - l.day_x(0) - 16.))
                .occlude()
                .flex()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_lg()
                .bg(theme.raised)
                .border_1()
                .border_color(theme.line_strong)
                .shadow_lg()
                .text_xs()
                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).flex_shrink_0().child("Outlook meetings aren't showing"))
                .child(div().flex_1().text_color(theme.muted).child(body))
                .when(settings, |d| {
                    d.child(
                        div()
                            .id("open-calendar-privacy")
                            .flex_shrink_0()
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .bg(theme.ink)
                            .text_color(theme.canvas)
                            .cursor_pointer()
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars");
                            }))
                            .child("Open Privacy & Security"),
                    )
                })
                .into_any_element(),
        )
    }
}

/// Logged time removed here that Jira still has until the next sync.
fn render_removed(theme: &Theme, issue: &IssueKey, range: TimeRange, g: Geom) -> AnyElement {
    div()
        .absolute()
        .left(px(g.x))
        .top(px(g.y))
        .w(px(g.w))
        .h(px(g.h))
        .rounded_md()
        .border_1()
        .border_dashed()
        .border_color(theme.danger)
        .px_1p5()
        .py_0p5()
        .text_xs()
        .text_color(theme.danger)
        .child(div().line_through().child(format!("{issue} {}", format_hours(range.duration()))))
        .when(g.h > 34., |d| d.child(div().text_color(theme.muted).child("Removed from Jira when you sync")))
        .into_any_element()
}

/// A meeting's or timer's title with its issue keys taken out, or nothing if that leaves
/// no words: "APP-123 review" is "review", and "APP-123" or "APP-123:" is nothing.
fn title_without_keys(title: &str) -> Option<String> {
    let mut rest = title.to_owned();
    for key in backfill_core::issue::find_issue_keys(title) {
        rest = rest.replace(key.as_str(), " ");
    }
    let rest = rest.split_whitespace().collect::<Vec<_>>().join(" ");
    let rest = rest.trim_matches(|c: char| !c.is_alphanumeric() && !"()\"'".contains(c)).to_owned();
    rest.chars().any(char::is_alphanumeric).then_some(rest)
}

fn snap(minutes: f32) -> f32 {
    (minutes / SNAP).round() * SNAP
}

#[allow(clippy::too_many_arguments)]
fn edge_pill(theme: &Theme, id: (&'static str, usize), x: f32, col_w: f32, y: f32, label: String, scroll_to: f32, cx: &mut Context<Backfill>) -> AnyElement {
    div()
        .id(id)
        .occlude()
        .absolute()
        .left(px(x + col_w / 2. - 50.))
        .top(px(y))
        .w(px(100.))
        .h(px(22.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme.raised)
        .border_1()
        .border_color(theme.line_strong)
        .text_xs()
        .text_color(theme.muted)
        .cursor_pointer()
        .hover(|s| s.text_color(theme.ink))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.scroll_into_view(scroll_to.max(0.));
            cx.notify();
        }))
        .child(label)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::title_without_keys;

    #[test]
    fn a_title_that_only_repeats_its_key_says_nothing() {
        assert_eq!(title_without_keys("APP-123"), None);
        assert_eq!(title_without_keys("APP-123: "), None);
        assert_eq!(title_without_keys("APP-123 review"), Some("review".into()));
        assert_eq!(title_without_keys("Workshop APP-7/APP-8"), Some("Workshop".into()));
        assert_eq!(title_without_keys("Team standup"), Some("Team standup".into()));
    }
}
