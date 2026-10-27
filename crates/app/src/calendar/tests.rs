use std::time::Duration;

use backfill_core::{demo, workspace::SyncState};
use gpui::{MouseButton, TestApp, TestAppWindow, point, px};
use gpui_elements::editable_text::actions::{DEFAULT_INPUT_CONTEXT, default_bindings};
use jiff::civil::date;

use super::*;

/// A Wednesday: last week is logged, and Monday and Tuesday are left to backfill.
const TODAY: Date = date(2026, 10, 28);
const MONDAY: Date = date(2026, 10, 26);
const TUESDAY: Date = date(2026, 10, 27);

fn open() -> (TestApp, TestAppWindow<Backfill>) {
    let mut app = TestApp::new();
    app.update(|cx| {
        cx.set_reduce_motion(true);
        cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
        bind_keys(cx);
        picker::bind_keys(cx);
    });
    let mut window = app.open_window(|window, cx| {
        Backfill::with_workspace(demo::workspace(TODAY, TimeZone::UTC), TODAY, window, cx)
    });
    window.draw();
    (app, window)
}

fn layout(window: &TestAppWindow<Backfill>) -> Layout {
    window.read(|this, _| this.layout.expect("drawn"))
}

/// The window position of `minutes` past midnight, `dx` into a day's column.
fn at(window: &TestAppWindow<Backfill>, day: usize, minutes: f32, dx: f32) -> Point<Pixels> {
    window.read(|this, _| {
        let l = this.layout.expect("drawn");
        point(px(l.day_x(day) + dx), px(l.cal_top + this.calendar_y(&l, minutes)))
    })
}

fn geom(window: &TestAppWindow<Backfill>, key: BlockKey) -> Geom {
    window.read(|this, _| this.rendered.iter().find(|(k, _)| *k == key).map(|(_, g)| *g).expect("drawn"))
}

fn worklogs_on(window: &TestAppWindow<Backfill>, date: Date) -> Vec<(String, String)> {
    window.read(|this, _| {
        let mut logs: Vec<_> = this.ws.worklogs_on(date).map(|w| (w.issue.to_string(), this.fmt_range(&w.range))).collect();
        logs.sort_by(|a, b| a.1.cmp(&b.1));
        logs
    })
}

#[test]
fn opens_on_this_week_midweek() {
    let (_app, window) = open();
    window.read(|this, _| assert_eq!(this.week, MONDAY));
}

#[test]
fn n_then_f_fills_the_first_unfinished_day() {
    let (_app, mut window) = open();
    window.simulate_keystrokes("n");
    window.read(|this, _| assert_eq!(this.selected, Some(Selected::Day(MONDAY))));
    window.simulate_keystrokes("f");
    window.draw();
    window.simulate_keystrokes("enter");
    window.read(|this, _| assert_eq!(this.ws.logged_on(MONDAY), this.ws.settings.target));
}

#[test]
fn undo_takes_a_fill_back() {
    let (_app, mut window) = open();
    window.simulate_keystrokes("n f");
    window.draw();
    window.simulate_keystrokes("enter secondary-z");
    window.read(|this, _| assert_eq!(this.ws.logged_on(MONDAY), SignedDuration::ZERO));
}

#[test]
fn typing_a_number_in_the_picker_finds_the_issue() {
    let (_app, mut window) = open();
    window.simulate_keystrokes("n f");
    window.draw();
    window.read(|this, _| assert!(this.picker.is_some()));
    window.simulate_input("104");
    window.simulate_keystrokes("enter");
    window.read(|this, _| assert!(this.picker.is_none()));
    assert!(worklogs_on(&window, MONDAY).iter().all(|(issue, _)| issue == "DEMO-104"));
}

#[test]
fn letters_typed_into_the_search_never_trigger_shortcuts() {
    let (_app, mut window) = open();
    window.simulate_keystrokes("n l");
    window.draw();
    // f fills, l logs, a absorbs, n moves on: none of them may fire while typing.
    window.simulate_input("flan");
    window.read(|this, _| {
        assert!(this.picker.is_some());
        assert_eq!(this.ws.logged_on(MONDAY), SignedDuration::ZERO);
        assert_eq!(this.selected, Some(Selected::Day(MONDAY)));
    });
    let query = window.read(|this, cx| this.picker.as_ref().unwrap().entity.read(cx).query_text(cx));
    assert_eq!(query, "flan");
}

#[test]
fn arrow_keys_move_the_picker_selection_rather_than_the_caret() {
    let (_app, mut window) = open();
    window.simulate_keystrokes("n l");
    window.draw();
    let second = window.read(|this, _| {
        let (h, m) = this.ws.settings.day_start;
        let workday = TimeRange::starting_at(time::at(MONDAY, h, m, &this.ws.tz), this.ws.settings.target);
        this.ws.suggestions(workday, None)[1].key.to_string()
    });
    window.simulate_keystrokes("down enter");
    assert!(worklogs_on(&window, MONDAY).iter().all(|(issue, _)| *issue == second), "expected {second}");
}

#[test]
fn escape_closes_the_picker_without_logging() {
    let (_app, mut window) = open();
    window.simulate_keystrokes("n l");
    window.draw();
    window.simulate_keystrokes("escape");
    window.read(|this, _| {
        assert!(this.picker.is_none());
        assert_eq!(this.ws.logged_on(MONDAY), SignedDuration::ZERO);
    });
}

#[test]
fn drawing_on_the_calendar_logs_that_time() {
    let (_app, mut window) = open();
    window.simulate_mouse_down(at(&window, 0, 9. * 60., 40.), MouseButton::Left);
    window.simulate_mouse_move(at(&window, 0, 10. * 60. + 50., 40.));
    window.simulate_mouse_up(at(&window, 0, 10. * 60. + 50., 40.), MouseButton::Left);
    window.read(|this, _| {
        assert!(matches!(this.picker.as_ref().map(|p| &p.target), Some(PickTarget::Drafts { fill: None })));
        assert_eq!(this.drafts.iter().map(|r| this.fmt_range(r)).collect::<Vec<_>>(), ["09:00–10:45"]);
    });
    window.draw();
    window.simulate_keystrokes("enter");
    assert_eq!(worklogs_on(&window, MONDAY).len(), 1);
    assert_eq!(worklogs_on(&window, MONDAY)[0].1, "09:00–10:45");
    window.read(|this, _| assert!(matches!(this.selected, Some(Selected::Worklog(_)))));
}

#[test]
fn dragging_a_block_moves_it_on_the_quarter_hour() {
    let (_app, mut window) = open();
    fill_monday(&mut window);
    window.draw();
    let before = worklogs_on(&window, MONDAY);
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].1, "08:00–15:30");
    window.simulate_mouse_down(at(&window, 0, 10. * 60. + 30., 8.), MouseButton::Left);
    window.simulate_mouse_move(at(&window, 0, 11. * 60. + 32., 8.));
    window.simulate_mouse_up(at(&window, 0, 11. * 60. + 32., 8.), MouseButton::Left);
    assert_eq!(worklogs_on(&window, MONDAY)[0].1, "09:00–16:30");
}

#[test]
fn dragging_the_bottom_edge_resizes() {
    let (_app, mut window) = open();
    fill_monday(&mut window);
    window.draw();
    let id = window.read(|this, _| this.ws.worklogs_on(MONDAY).next().unwrap().id);
    let g = geom(&window, BlockKey::Worklog(id.0));
    // Press on the block's bottom edge, as drawn.
    window.simulate_mouse_down(point(px(g.x + 8.), px(g.y + g.h - 2.)), MouseButton::Left);
    window.simulate_mouse_move(at(&window, 0, 16. * 60., 8.));
    window.simulate_mouse_up(at(&window, 0, 16. * 60., 8.), MouseButton::Left);
    assert_eq!(worklogs_on(&window, MONDAY)[0].1, "08:00–16:00");
}

#[test]
fn the_log_button_logs_a_meeting_on_the_issue_it_names() {
    let (_app, mut window) = open();
    let workshop = window.read(|this, _| {
        this.ws.activities_on(TUESDAY).find(|a| a.title.starts_with("Workshop")).map(|a| a.id).expect("demo workshop")
    });
    let g = geom(&window, BlockKey::Activity(workshop.0));
    window.simulate_mouse_move(point(px(g.x + g.w / 2.), px(g.y + g.h / 2.)));
    window.draw();
    window.simulate_click(point(px(g.x + g.w - 12.), px(g.y + 10.)), MouseButton::Left);
    window.read(|this, _| {
        let logged = this.ws.worklog_for(workshop).expect("logged");
        assert_eq!(logged.issue.as_str(), "DEMO-120");
        assert_eq!(this.fmt_range(&logged.range), "12:30–14:30");
    });
}

/// Fills Monday on the issue suggested first, then clears the selection.
fn fill_monday(window: &mut TestAppWindow<Backfill>) {
    window.simulate_keystrokes("n f");
    window.draw();
    window.simulate_keystrokes("enter escape");
    window.draw();
}

fn rendered(window: &TestAppWindow<Backfill>, key: BlockKey) -> bool {
    window.read(|this, _| this.rendered.iter().any(|(k, _)| *k == key))
}

fn activity_on(window: &TestAppWindow<Backfill>, date: Date, title: &str) -> ActivityId {
    window.read(|this, _| this.ws.activities_on(date).find(|a| a.title.starts_with(title)).map(|a| a.id).expect("demo meeting"))
}

#[test]
fn meetings_covered_by_logged_time_are_not_drawn() {
    let (_app, mut window) = open();
    let standup = activity_on(&window, MONDAY, "Daily standup");
    assert!(rendered(&window, BlockKey::Activity(standup.0)));
    fill_monday(&mut window);
    assert!(!rendered(&window, BlockKey::Activity(standup.0)));
}

#[test]
fn dragging_from_a_meeting_still_logs_time() {
    let (_app, mut window) = open();
    let workshop = activity_on(&window, TUESDAY, "Workshop");
    let g = geom(&window, BlockKey::Activity(workshop.0));
    let start = at(&window, 1, 13. * 60., 0.).y;
    let end = at(&window, 1, 14. * 60., 0.).y;
    let x = px(g.x + 20.);
    window.simulate_mouse_down(point(x, start), MouseButton::Left);
    window.simulate_mouse_move(point(x, end));
    window.simulate_mouse_up(point(x, end), MouseButton::Left);
    window.read(|this, _| {
        assert!(matches!(this.picker.as_ref().map(|p| &p.target), Some(PickTarget::Drafts { fill: None })));
        assert_eq!(this.drafts.iter().map(|r| this.fmt_range(r)).collect::<Vec<_>>(), ["13:00–14:00"]);
    });
}

#[test]
fn clicking_a_meeting_selects_it() {
    let (_app, mut window) = open();
    let workshop = activity_on(&window, TUESDAY, "Workshop");
    let g = geom(&window, BlockKey::Activity(workshop.0));
    window.simulate_click(point(px(g.x + 20.), px(g.y + g.h / 2.)), MouseButton::Left);
    window.read(|this, _| {
        assert_eq!(this.selected, Some(Selected::Activity(workshop)));
        assert!(this.picker.is_none());
    });
}

#[test]
fn long_events_go_in_the_day_heading_instead_of_the_grid() {
    let (_app, mut window) = open();
    let id = window.update(|this, _, cx| {
        let id = ActivityId(this.ws.next_id());
        this.ws.activities.push(Activity {
            id,
            source: Source::Outlook,
            title: "Away day".into(),
            range: TimeRange::new(time::at(TUESDAY, 7, 0, &this.ws.tz), time::at(TUESDAY, 16, 0, &this.ws.tz)),
            series: None,
            absorbed: false,
        });
        cx.notify();
        id
    });
    window.draw();
    assert!(!rendered(&window, BlockKey::Activity(id.0)));
    window.read(|this, _| {
        assert_eq!(this.long_events(TUESDAY).iter().map(|a| a.id).collect::<Vec<_>>(), [id]);
        assert_eq!(this.banners().len(), 1);
    });
    // The grid is still free to draw on.
    window.simulate_mouse_down(at(&window, 1, 9. * 60., 30.), MouseButton::Left);
    window.simulate_mouse_move(at(&window, 1, 10. * 60., 30.));
    window.simulate_mouse_up(at(&window, 1, 10. * 60., 30.), MouseButton::Left);
    window.read(|this, _| assert!(matches!(this.picker.as_ref().map(|p| &p.target), Some(PickTarget::Drafts { fill: None }))));
}

#[test]
fn clicking_a_day_heading_selects_the_day() {
    let (_app, mut window) = open();
    let l = layout(&window);
    window.simulate_click(point(px(l.day_x(1) + 20.), px(HEADER_H + 20.)), MouseButton::Left);
    window.read(|this, _| assert_eq!(this.selected, Some(Selected::Day(TUESDAY))));
}

#[test]
fn the_calendar_scrolls_through_the_whole_day() {
    let (_app, mut window) = open();
    window.read(|this, _| assert_eq!(this.scroll_shown, FOCUS_START));
    let inside = at(&window, 0, 12. * 60., 40.);
    window.simulate_scroll(inside, point(px(0.), px(2000.)));
    window.draw();
    window.read(|this, _| assert_eq!(this.scroll_shown, 0.));
    window.simulate_scroll(inside, point(px(0.), px(-20000.)));
    window.draw();
    window.read(|this, _| {
        let l = this.layout.unwrap();
        assert!((this.scroll_shown + l.visible_hours() - 24.).abs() < 0.01);
    });
}

#[test]
fn header_buttons_still_click() {
    let (_app, mut window) = open();
    // The "›" button: after the traffic lights and the "‹" button.
    let x = if cfg!(target_os = "macos") { 84. } else { 16. } + 28. + 4. + 14.;
    window.simulate_click(point(px(x), px(HEADER_H / 2.)), MouseButton::Left);
    window.read(|this, _| assert_eq!(this.week, add_days(MONDAY, 7)));
}

#[test]
fn sync_review_writes_every_change() {
    let (app, mut window) = open();
    window.simulate_keystrokes("secondary-enter");
    window.draw();
    window.read(|this, _| assert_eq!(this.sheet.as_ref().map(|s| s.ops.len()), Some(2)));
    // A plain Enter doesn't write to Jira.
    window.simulate_keystrokes("enter");
    app.run_until_parked();
    window.read(|this, _| assert_eq!(this.ws.sync_plan().len(), 2));
    window.simulate_keystrokes("secondary-enter");
    for _ in 0..10 {
        app.advance_clock(Duration::from_millis(400));
        app.run_until_parked();
    }
    window.read(|this, _| {
        assert!(this.ws.sync_plan().is_empty());
        assert!(this.sheet.as_ref().is_some_and(|s| s.finished));
    });
    window.simulate_keystrokes("secondary-enter");
    window.read(|this, _| assert!(this.sheet.is_none()));
}

#[test]
fn restarting_to_update_waits_for_a_running_sync() {
    let (app, mut window) = open();
    window.update(|this, _, cx| {
        this.update = update::Update::Ready(backfill_update::Version(0, 2, 0));
        *this.pending_update.0.borrow_mut() = Some(("/updates/backfill.app".into(), "/Applications/backfill.app".into()));
        cx.notify();
    });
    window.simulate_keystrokes("secondary-enter");
    app.run_until_parked();
    window.simulate_keystrokes("secondary-enter");
    window.read(|this, _| assert!(this.sheet.as_ref().is_some_and(|s| s.running)));
    window.update(|this, _, cx| this.restart_to_update(cx));
    window.read(|this, _| {
        assert_eq!(this.toast.as_ref().map(|t| t.message.as_ref()), Some("backfill can restart once the sync is done"));
        // Still waiting to be installed.
        assert!(this.pending_update.0.borrow().is_some());
    });
}

#[test]
fn command_o_opens_the_selected_issue_in_jira() {
    let (app, mut window) = open();
    let key = window.update(|this, _, _| {
        this.settings.jira_site = "example.atlassian.net".into();
        let worklog = this.ws.worklogs.first().expect("last week is logged");
        this.selected = Some(Selected::Worklog(worklog.id));
        worklog.issue.to_string()
    });
    window.simulate_keystrokes("o");
    assert_eq!(app.opened_url(), None);
    window.simulate_keystrokes("secondary-o");
    assert_eq!(app.opened_url(), Some(format!("https://example.atlassian.net/browse/{key}")));
}

#[test]
fn command_o_opens_the_highlighted_search_result_and_a_typed_o_is_just_text() {
    let (app, mut window) = open();
    window.update(|this, _, _| this.settings.jira_site = "example.atlassian.net".into());
    window.simulate_keystrokes("n l");
    window.draw();
    window.simulate_input("o");
    assert_eq!(app.opened_url(), None);
    window.simulate_keystrokes("backspace");
    let key = window.read(|this, cx| this.picker.as_ref().unwrap().entity.read(cx).highlighted()).expect("a suggestion");
    window.simulate_keystrokes("secondary-o");
    assert_eq!(app.opened_url(), Some(format!("https://example.atlassian.net/browse/{key}")));
}

#[test]
fn opening_in_jira_without_a_site_says_where_to_add_it() {
    let (app, mut window) = open();
    window.update(|this, _, _| {
        let worklog = this.ws.worklogs.first().expect("last week is logged");
        this.selected = Some(Selected::Worklog(worklog.id));
    });
    window.simulate_keystrokes("secondary-o");
    assert_eq!(app.opened_url(), None);
    window.read(|this, _| assert_eq!(this.toast.as_ref().map(|t| t.message.as_ref()), Some("Add your Jira site in Settings to open issues")));
}

fn right_click(window: &mut TestAppWindow<Backfill>, key: BlockKey) {
    let g = geom(window, key);
    let position = point(px(g.x + 20.), px(g.y + g.h / 2.));
    window.simulate_mouse_down(position, MouseButton::Right);
    window.simulate_mouse_up(position, MouseButton::Right);
    window.draw();
}

#[test]
fn right_clicking_a_logged_block_offers_what_can_be_done_to_it() {
    let (_app, mut window) = open();
    fill_monday(&mut window);
    window.draw();
    let (id, issue) = window.read(|this, _| {
        let worklog = this.ws.worklogs_on(MONDAY).next().unwrap();
        (worklog.id, worklog.issue.to_string())
    });
    right_click(&mut window, BlockKey::Worklog(id.0));
    window.read(|this, _| {
        assert_eq!(this.selected, Some(Selected::Worklog(id)));
        assert_eq!(this.menu_labels(), ["Move to another issue…".to_owned(), format!("Open {issue} in Jira"), "Remove".to_owned()]);
    });
    // Up from nothing highlights the last row, which removes the block.
    window.simulate_keystrokes("up enter");
    assert!(worklogs_on(&window, MONDAY).is_empty());
    window.read(|this, _| assert!(this.menu.is_none()));
}

#[test]
fn right_clicking_a_meeting_can_make_it_part_of_the_main_task() {
    let (_app, mut window) = open();
    let workshop = activity_on(&window, TUESDAY, "Workshop");
    right_click(&mut window, BlockKey::Activity(workshop.0));
    let labels = window.read(|this, _| this.menu_labels());
    let row = labels.iter().position(|l| l == "Part of the main task").expect("an absorb row");
    window.simulate_keystrokes(&vec!["down"; row + 1].join(" "));
    window.simulate_keystrokes("enter");
    window.read(|this, _| {
        assert!(this.ws.activity(workshop).unwrap().absorbed);
        assert!(this.menu.is_none());
    });
    right_click(&mut window, BlockKey::Activity(workshop.0));
    window.read(|this, _| assert!(this.menu_labels().contains(&"✓ Part of the main task".to_owned())));
}

#[test]
fn escape_closes_the_menu_and_keeps_the_selection() {
    let (_app, mut window) = open();
    let workshop = activity_on(&window, TUESDAY, "Workshop");
    right_click(&mut window, BlockKey::Activity(workshop.0));
    window.read(|this, _| assert!(this.menu.is_some()));
    window.simulate_keystrokes("escape");
    window.read(|this, _| {
        assert!(this.menu.is_none());
        assert_eq!(this.selected, Some(Selected::Activity(workshop)));
    });
}

#[test]
fn control_click_opens_the_menu_too() {
    let (_app, mut window) = open();
    let workshop = activity_on(&window, TUESDAY, "Workshop");
    let g = geom(&window, BlockKey::Activity(workshop.0));
    window.simulate_event(gpui::MouseDownEvent {
        button: MouseButton::Left,
        position: point(px(g.x + 20.), px(g.y + g.h / 2.)),
        modifiers: gpui::Modifiers::control(),
        click_count: 1,
        first_mouse: false,
    });
    window.read(|this, _| {
        assert!(this.menu.is_some());
        assert!(this.drag.is_none());
    });
}

#[test]
fn moving_a_block_offers_the_other_issues() {
    let (_app, mut window) = open();
    fill_monday(&mut window);
    window.draw();
    let (id, issue) = window.read(|this, _| {
        let worklog = this.ws.worklogs_on(MONDAY).next().unwrap();
        (worklog.id, worklog.issue.to_string())
    });
    window.update(|this, _, _| this.selected = Some(Selected::Worklog(id)));
    window.simulate_keystrokes("l");
    window.draw();
    let listed = window.read(|this, cx| this.picker.as_ref().unwrap().entity.read(cx).listed());
    assert!(!listed.is_empty() && !listed.contains(&issue));
}

#[test]
fn fill_drafts_the_gaps_and_logs_them_once_an_issue_is_picked() {
    let (_app, mut window) = open();
    window.simulate_keystrokes("n f");
    window.draw();
    window.read(|this, _| {
        assert_eq!(this.drafts, this.ws.fill_plan(MONDAY).gaps);
        assert!(!this.drafts.is_empty());
        assert_eq!(this.ws.logged_on(MONDAY), SignedDuration::ZERO);
        assert!(matches!(this.picker.as_ref().map(|p| &p.target), Some(PickTarget::Drafts { fill: Some(MONDAY) })));
    });
    window.simulate_keystrokes("escape");
    window.read(|this, _| {
        assert!(this.drafts.is_empty() && this.picker.is_none());
        assert_eq!(this.ws.logged_on(MONDAY), SignedDuration::ZERO);
    });
}

#[test]
fn a_draft_can_be_resized_and_moved_before_its_issue_is_picked() {
    let (_app, mut window) = open();
    window.simulate_mouse_down(at(&window, 2, 9. * 60., 40.), MouseButton::Left);
    window.simulate_mouse_move(at(&window, 2, 10. * 60., 40.));
    window.simulate_mouse_up(at(&window, 2, 10. * 60., 40.), MouseButton::Left);
    window.draw();
    let wednesday = add_days(MONDAY, 2);

    // Stretch it from its bottom edge to 10:30.
    let g = geom(&window, BlockKey::Draft(0));
    window.simulate_mouse_down(point(px(g.x + 8.), px(g.y + g.h - 2.)), MouseButton::Left);
    window.simulate_mouse_move(at(&window, 2, 10. * 60. + 30., 8.));
    window.simulate_mouse_up(at(&window, 2, 10. * 60. + 30., 8.), MouseButton::Left);
    window.draw();
    // Then carry it down an hour by its middle.
    let grab = at(&window, 2, 9. * 60. + 45., 30.);
    window.simulate_mouse_down(grab, MouseButton::Left);
    window.simulate_mouse_move(point(grab.x, grab.y + px(layout(&window).hour_h)));
    window.simulate_mouse_up(point(grab.x, grab.y + px(layout(&window).hour_h)), MouseButton::Left);
    window.draw();
    window.read(|this, cx| {
        assert!(this.picker.is_some(), "the picker stays open while the draft is edited");
        assert_eq!(this.drafts.iter().map(|r| this.fmt_range(r)).collect::<Vec<_>>(), ["10:00–11:30"]);
        assert_eq!(this.picker.as_ref().unwrap().entity.read(cx).title(), "Log 10:00–11:30 (1.5h)");
    });
    window.simulate_keystrokes("enter");
    assert_eq!(worklogs_on(&window, wednesday).iter().map(|(_, r)| r.as_str()).collect::<Vec<_>>(), ["10:00–11:30"]);
}

#[test]
fn blocks_make_room_while_time_is_drawn_over_them() {
    let (_app, mut window) = open();
    let workshop = activity_on(&window, TUESDAY, "Workshop");
    let before = geom(&window, BlockKey::Activity(workshop.0)).w;
    let start = at(&window, 1, 12. * 60., 4.);
    window.simulate_mouse_down(point(start.x, start.y), MouseButton::Left);
    window.simulate_mouse_move(at(&window, 1, 14. * 60., 4.));
    window.draw();
    let during = geom(&window, BlockKey::Activity(workshop.0)).w;
    assert!(during < before * 0.6, "the meeting should share its column with the drawn time ({during} of {before})");
    assert!(rendered(&window, BlockKey::Draft(0)));
}

#[test]
fn right_clicking_a_day_heading_offers_to_fill_it() {
    let (_app, mut window) = open();
    let l = layout(&window);
    let position = point(px(l.day_x(0) + 20.), px(HEADER_H + 20.));
    window.simulate_mouse_down(position, MouseButton::Right);
    window.simulate_mouse_up(position, MouseButton::Right);
    window.draw();
    window.read(|this, _| {
        assert_eq!(this.selected, Some(Selected::Day(MONDAY)));
        assert_eq!(this.menu_labels(), ["Fill the day…"]);
    });
    window.simulate_keystrokes("down enter");
    window.draw();
    window.read(|this, _| assert!(matches!(this.picker.as_ref().map(|p| &p.target), Some(PickTarget::Drafts { fill: Some(MONDAY) }))));
}

#[test]
fn the_review_can_drop_one_change_and_keep_the_rest() {
    let (_app, mut window) = open();
    window.simulate_keystrokes("secondary-enter");
    window.draw();
    let kept = window.update(|this, window, cx| {
        let ops = this.sheet.as_ref().unwrap().ops.clone();
        assert_eq!(ops.len(), 2);
        this.undo_change(0, window, cx);
        assert_eq!(this.sheet.as_ref().unwrap().ops, ops[1..]);
        assert_eq!(this.ws.sync_plan(), ops[1..]);
        ops[1].clone()
    });
    window.update(|this, window, cx| this.undo_change(0, window, cx));
    window.read(|this, _| {
        assert!(this.sheet.is_none() && this.ws.sync_plan().is_empty());
        assert_eq!(this.toast.as_ref().map(|t| t.message.as_ref()), Some("Everything here matches Jira"));
    });
    window.simulate_keystrokes("secondary-z");
    window.read(|this, _| assert_eq!(this.ws.sync_plan(), [kept]));
}

#[test]
fn blocks_say_how_they_differ_from_jira() {
    let (_app, mut window) = open();
    window.update(|this, _, _| {
        let pulled = this.ws.worklogs.iter().find(|w| this.ws.sync_state(w) == SyncState::Synced).unwrap().clone();
        let hour = SignedDuration::from_hours(1);
        let half = SignedDuration::from_mins(30);
        let label = |this: &Backfill, w: &Worklog| this.difference(w).map(|(text, _)| text);
        assert_eq!(label(this, &pulled), None);
        let longer = Worklog { range: TimeRange::new(pulled.range.start, pulled.range.end + half), ..pulled.clone() };
        assert_eq!(label(this, &longer).as_deref(), Some("+30m"));
        let later = Worklog { range: pulled.range.shifted(hour), ..pulled.clone() };
        assert_eq!(label(this, &later).as_deref(), Some("1h later"));
        let elsewhere = Worklog { issue: IssueKey::parse("DEMO-6").unwrap(), range: TimeRange::new(pulled.range.start, pulled.range.end - half), ..pulled.clone() };
        assert_eq!(label(this, &elsewhere), Some(format!("from {}, −30m", pulled.issue)));
        let new = Worklog { remote_id: None, ..pulled };
        assert_eq!(this.difference(&new), Some(("New".to_owned(), true)));
    });
}

#[test]
fn the_review_opens_at_once_but_syncs_only_after_jira_is_checked() {
    let (app, mut window) = open();
    window.update(|this, window, cx| this.open_sheet(Check::Running, window, cx));
    window.draw();
    window.simulate_keystrokes("secondary-enter");
    app.run_until_parked();
    window.read(|this, _| {
        let sheet = this.sheet.as_ref().expect("the review is open while Jira is checked");
        assert!(!sheet.running && sheet.check == Check::Running);
        assert_eq!(this.ws.sync_plan().len(), 2);
    });

    window.update(|this, window, cx| this.finish_check(Err("Jira didn't answer".into()), window, cx));
    window.simulate_keystrokes("secondary-enter");
    window.read(|this, _| {
        assert_eq!(this.sheet.as_ref().unwrap().check, Check::Failed("Jira didn't answer".into()));
        assert_eq!(this.ws.sync_plan().len(), 2, "nothing is synced after a failed check");
    });

    window.update(|this, window, cx| this.finish_check(Ok(()), window, cx));
    window.simulate_keystrokes("secondary-enter");
    for _ in 0..10 {
        app.advance_clock(Duration::from_millis(400));
        app.run_until_parked();
    }
    window.read(|this, _| assert!(this.ws.sync_plan().is_empty()));
}

#[test]
fn double_clicking_a_day_heading_fills_it() {
    let (_app, mut window) = open();
    let l = layout(&window);
    let position = point(px(l.day_x(0) + 20.), px(HEADER_H + 20.));
    window.simulate_event(gpui::MouseDownEvent { button: MouseButton::Left, position, modifiers: gpui::Modifiers::default(), click_count: 1, first_mouse: false });
    window.simulate_event(gpui::MouseUpEvent { button: MouseButton::Left, position, modifiers: gpui::Modifiers::default(), click_count: 1 });
    window.simulate_event(gpui::MouseDownEvent { button: MouseButton::Left, position, modifiers: gpui::Modifiers::default(), click_count: 2, first_mouse: false });
    window.simulate_event(gpui::MouseUpEvent { button: MouseButton::Left, position, modifiers: gpui::Modifiers::default(), click_count: 2 });
    window.read(|this, _| assert!(matches!(this.picker.as_ref().map(|p| &p.target), Some(PickTarget::Drafts { fill: Some(MONDAY) }))));
}

#[test]
fn a_meeting_over_two_days_shows_and_logs_each_days_part() {
    let (_app, mut window) = open();
    let wednesday = add_days(MONDAY, 2);
    let thursday = add_days(MONDAY, 3);
    let id = window.update(|this, _, cx| {
        let id = ActivityId(this.ws.next_id());
        this.ws.activities.push(Activity {
            id,
            source: Source::Outlook,
            title: "Team gathering".into(),
            range: TimeRange::new(time::at(wednesday, 9, 0, &this.ws.tz), time::at(thursday, 16, 30, &this.ws.tz)),
            series: None,
            absorbed: false,
        });
        cx.notify();
        id
    });
    window.draw();
    window.read(|this, _| {
        assert_eq!(this.long_events(wednesday).iter().map(|a| a.id).collect::<Vec<_>>(), [id]);
        assert_eq!(this.long_events(thursday).iter().map(|a| a.id).collect::<Vec<_>>(), [id]);
    });
    window.update(|this, _, cx| this.apply_pick(PickTarget::Activity(id), IssueKey::parse("DEMO-104").unwrap(), cx));
    assert_eq!(worklogs_on(&window, wednesday), [("DEMO-104".to_owned(), "09:00–16:30".to_owned())]);
    assert_eq!(worklogs_on(&window, thursday), [("DEMO-104".to_owned(), "09:00–16:30".to_owned())]);
}

#[test]
fn a_meeting_over_two_days_is_one_bar_across_them_and_not_in_the_grid() {
    let (_app, mut window) = open();
    let wednesday = add_days(MONDAY, 2);
    let thursday = add_days(MONDAY, 3);
    // Wednesday 09:00 to Thursday 13:00, like a hackathon.
    let id = window.update(|this, _, cx| {
        let id = ActivityId(this.ws.next_id());
        this.ws.activities.push(Activity {
            id,
            source: Source::Outlook,
            title: "Hackathon".into(),
            range: TimeRange::new(time::at(wednesday, 9, 0, &this.ws.tz), time::at(thursday, 13, 0, &this.ws.tz)),
            series: None,
            absorbed: false,
        });
        cx.notify();
        id
    });
    window.draw();
    assert!(!rendered(&window, BlockKey::Activity(id.0)));
    window.read(|this, _| {
        let banners = this.banners();
        let bar = banners.iter().find(|b| b.activity.id == id).expect("a bar under the headings");
        assert_eq!((bar.first, bar.last, bar.lane), (2, 3, 0));
    });
}

#[test]
fn overlapping_bars_get_lanes_of_their_own() {
    let (_app, mut window) = open();
    let tuesday = TUESDAY;
    let thursday = add_days(MONDAY, 3);
    window.update(|this, _, cx| {
        for (title, from, to) in [("Offsite", tuesday, thursday), ("Conference", thursday, add_days(thursday, 1))] {
            let id = ActivityId(this.ws.next_id());
            this.ws.activities.push(Activity {
                id,
                source: Source::Outlook,
                title: title.into(),
                range: TimeRange::new(time::at(from, 9, 0, &this.ws.tz), time::at(to, 13, 0, &this.ws.tz)),
                series: None,
                absorbed: false,
            });
        }
        cx.notify();
    });
    window.draw();
    window.read(|this, _| {
        let lanes: Vec<(String, usize)> = this.banners().iter().map(|b| (b.activity.title.clone(), b.lane)).collect();
        assert_eq!(lanes, [("Offsite".to_owned(), 0), ("Conference".to_owned(), 1)]);
    });
}

#[test]
fn resizing_the_window_moves_blocks_with_the_grid_at_once() {
    let (mut app, mut window) = open();
    app.update(|cx| cx.set_reduce_motion(false));
    let workshop = activity_on(&window, TUESDAY, "Workshop");
    let start = window.read(|this, _| this.minutes(this.ws.activity(workshop).unwrap().range.start, 1));
    let before = layout(&window);
    window.simulate_resize(gpui::size(px(1700.), px(1100.)));
    window.draw();
    let after = layout(&window);
    assert!(after.col_w != before.col_w && after.hour_h != before.hour_h);
    let g = geom(&window, BlockKey::Activity(workshop.0));
    let corner = at(&window, 1, start, 2.);
    assert_eq!((g.x, g.y), (f32::from(corner.x), f32::from(corner.y)), "the block should already be at its place in the resized grid");
}
