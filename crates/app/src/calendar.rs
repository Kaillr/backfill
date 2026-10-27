//! The week, as a calendar. Logged time is solid blocks in the issue's colour; meetings
//! and timers you haven't logged are outlined blocks beside them, ready to log.

mod menu;
mod review;
mod status;
pub(crate) mod update;
mod week;

use std::time::Duration;

use std::collections::HashMap;

use backfill_core::{
    Activity, ActivityId, IssueKey, Source, TimeRange, Worklog, WorklogId, attribution,
    issue::find_issue_keys,
    sync::SyncOp,
    time::{self, add_days, format_hours, local_date, week_start},
    workspace::Workspace,
};
use backfill_jira::Account;
use gpui::{
    Animated, Animation, AnimationExt, App, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    Global, KeyBinding, MouseButton, MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, SharedString,
    Subscription, Task, TitlebarOptions, WeakEntity, Window, WindowBounds, WindowHandle, WindowOptions,
    actions, anchored, deferred, div, ease_out_quint, millis, point, prelude::*, px, relative, size, spring,
};
use jiff::{SignedDuration, Timestamp, Zoned, civil::Date, tz::TimeZone};

use crate::{
    motion::{BlockKey, Blocks, Geom, Spot},
    outlook,
    picker::{self, IssuePicker, PickerEvent, Row, Search},
    script,
    settings::{self, Pane, SettingsEvent, SettingsView, Status, Statuses},
    sources::{self, JiraIdentity},
    store::{Secret, Settings, Store},
    theme::{Theme, with_alpha},
};

actions!(
    backfill,
    [
        Link,
        FillDay,
        ToggleAbsorb,
        DeleteSelection,
        PrevWeek,
        NextWeek,
        ThisWeek,
        NextAttention,
        ReviewSync,
        ApplySync,
        Dismiss,
        Undo,
        OpenSettings,
        SyncNow,
        OpenInJira,
        MenuNext,
        MenuPrev,
        MenuConfirm,
    ]
);

const CONTEXT: &str = "Backfill";

pub fn bind_keys(cx: &mut App) {
    // Single keys only work outside text fields, so typing in the issue search never
    // triggers them.
    let shortcut = Some("Backfill && !EditableText");
    cx.bind_keys([
        KeyBinding::new("l", Link, shortcut),
        KeyBinding::new("enter", Link, shortcut),
        KeyBinding::new("f", FillDay, shortcut),
        KeyBinding::new("a", ToggleAbsorb, shortcut),
        KeyBinding::new("backspace", DeleteSelection, shortcut),
        KeyBinding::new("delete", DeleteSelection, shortcut),
        KeyBinding::new("left", PrevWeek, shortcut),
        KeyBinding::new("right", NextWeek, shortcut),
        KeyBinding::new("home", ThisWeek, shortcut),
        KeyBinding::new("n", NextAttention, shortcut),
        KeyBinding::new("secondary-z", Undo, shortcut),
        KeyBinding::new("secondary-o", OpenInJira, Some(CONTEXT)),
        KeyBinding::new("secondary-enter", ReviewSync, Some(CONTEXT)),
        KeyBinding::new("secondary-r", SyncNow, Some(CONTEXT)),
        KeyBinding::new("escape", Dismiss, Some(CONTEXT)),
        KeyBinding::new("down", MenuNext, Some(menu::CONTEXT)),
        KeyBinding::new("up", MenuPrev, Some(menu::CONTEXT)),
        KeyBinding::new("enter", MenuConfirm, Some(menu::CONTEXT)),
        // The same keys that open the review sync it, so a stray Enter never writes to Jira.
        KeyBinding::new("secondary-enter", ApplySync, Some("SyncSheet")),
        KeyBinding::new("escape", Dismiss, Some("SyncSheet")),
    ]);
}

// Window layout, in logical pixels.
const HEADER_H: f32 = 64.;
const DAYHEAD_H: f32 = 64.;
/// Room under the day headings for long events, when the week has any.
const LONG_ROW_H: f32 = 28.;
/// Each further lane of long events, when they overlap.
const LANE_H: f32 = 24.;
/// Lanes beyond this aren't drawn, so the calendar keeps its room.
const MAX_LANES: usize = 3;
/// Events at least this long go in the day heading instead of the grid, like all-day
/// events in Outlook, so they never cover the day.
const LONG_EVENT: SignedDuration = SignedDuration::from_hours(6);
const GUTTER_W: f32 = 60.;
const COL_GAP: f32 = 8.;
const RIGHT_PAD: f32 = 16.;
const FOOTER_H: f32 = 40.;
const BOTTOM_PAD: f32 = 8.;
const DAYS: usize = 5;
/// The calendar opens on these hours and sizes them to fill its height. The rest of the
/// day is a scroll away.
const FOCUS_START: f32 = 7.;
const FOCUS_HOURS: f32 = 10.;
const SHEET_W: f32 = 480.;

/// Where things are on screen this frame.
#[derive(Clone, Copy)]
struct Layout {
    width: f32,
    col_w: f32,
    cal_top: f32,
    cal_h: f32,
    hour_h: f32,
}

impl Layout {
    fn new(width: f32, height: f32, lanes: usize) -> Self {
        let col_w = ((width - GUTTER_W - RIGHT_PAD - COL_GAP * (DAYS - 1) as f32) / DAYS as f32).max(120.);
        let cal_top = HEADER_H + DAYHEAD_H + if lanes > 0 { LONG_ROW_H + (lanes - 1) as f32 * LANE_H } else { 0. };
        let cal_h = (height - cal_top - FOOTER_H - BOTTOM_PAD).max(120.);
        Self { width, col_w, cal_top, cal_h, hour_h: (cal_h / FOCUS_HOURS).max(22.) }
    }

    fn day_x(&self, day: usize) -> f32 {
        GUTTER_W + day as f32 * (self.col_w + COL_GAP)
    }

    /// Where a block at `spot` is drawn, with the calendar scrolled `scroll_px` down.
    fn block(&self, spot: Spot, scroll_px: f32) -> Geom {
        let inner = self.col_w - 4.;
        let gap = if spot.width < 0.999 { 2. } else { 0. };
        let top = spot.start / 60. * self.hour_h;
        let height = ((spot.end - spot.start) / 60. * self.hour_h - 1.).max(14.);
        Geom {
            x: GUTTER_W + spot.day * (self.col_w + COL_GAP) + 2. + spot.left * inner,
            y: top - spot.raised - scroll_px,
            w: spot.width * inner - gap,
            h: height * spot.grown,
            opacity: spot.opacity,
        }
    }

    /// The day column under a window x, if any.
    fn day_at(&self, x: f32) -> Option<usize> {
        let offset = x - GUTTER_W;
        if offset < 0. {
            return None;
        }
        let day = (offset / (self.col_w + COL_GAP)) as usize;
        let within = offset - day as f32 * (self.col_w + COL_GAP);
        (day < DAYS && within <= self.col_w).then_some(day)
    }

    /// How many hours fit in the calendar at once.
    fn visible_hours(&self) -> f32 {
        self.cal_h / self.hour_h
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Selected {
    Worklog(WorklogId),
    /// A meeting or timer that isn't logged.
    Activity(ActivityId),
    Day(Date),
}

#[derive(Clone, Debug)]
enum PickTarget {
    /// Log a meeting or timer on an issue.
    Activity(ActivityId),
    /// Move a block of logged time to another issue.
    Worklog(WorklogId),
    /// The drafts: time drawn on the calendar, or what Fill proposes for a day. Filling
    /// also logs that day's meetings that name an issue on their own issues.
    Drafts { fill: Option<Date> },
}

struct PickerState {
    entity: Entity<IssuePicker>,
    target: PickTarget,
    anchor: Point<Pixels>,
    _subscription: Subscription,
}

#[derive(Clone, Copy, PartialEq)]
enum Edge {
    Top,
    Bottom,
}

/// A block that can be moved and resized.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Held {
    Worklog(WorklogId),
    /// A draft, by its place in `drafts`.
    Draft(usize),
}

/// A pointer drag in the calendar, in minutes past midnight.
enum Drag {
    /// Drawing new time. `on` is the meeting the press started on, if any: a click without
    /// a drag selects it instead.
    Draw { day: usize, anchor: f32, current: f32, on: Option<ActivityId> },
    Move { held: Held, day: usize, start: f32, duration: f32, grab: f32, origin: Point<Pixels>, moved: bool },
    Resize { held: Held, day: usize, start: f32, end: f32, edge: Edge },
}

#[derive(Clone, PartialEq)]
enum OpStatus {
    Waiting,
    Working,
    Done,
    Failed(String),
}

/// The check with Jira that comes before a sync: the days the changes touch are read
/// again, so the review shows what Jira has now.
#[derive(Clone, Debug, PartialEq)]
enum Check {
    Running,
    Done,
    Failed(String),
}

struct Sheet {
    ops: Vec<SyncOp>,
    status: Vec<OpStatus>,
    /// Syncing waits for this to be done.
    check: Check,
    running: bool,
    finished: bool,
    focus: FocusHandle,
    _task: Option<Task<()>>,
}

struct Toast {
    message: SharedString,
    undo: bool,
    generation: u64,
    _task: Task<()>,
}

/// What undo restores. Syncing clears the history, since Jira can't be undone from here.
struct Snapshot {
    worklogs: Vec<Worklog>,
    activities: Vec<Activity>,
}

pub struct Backfill {
    ws: Workspace,
    today: Date,
    week: Date,
    selected: Option<Selected>,
    picker: Option<PickerState>,
    /// Time waiting for an issue while the picker is open: drawn, or proposed by Fill.
    /// They can be moved and resized before an issue is picked.
    drafts: Vec<TimeRange>,
    sheet: Option<Sheet>,
    drag: Option<Drag>,
    blocks: Blocks,
    /// Calendar geometry drawn last frame, in window coordinates.
    rendered: Vec<(BlockKey, Geom)>,
    layout: Option<Layout>,
    /// The hour at the top of the calendar, animated when it scrolls on its own.
    scroll: Animated<f32>,
    scroll_shown: f32,
    /// Set when the week changes, so the next frame's blocks arrive day by day.
    entering_week: bool,
    history: Vec<Snapshot>,
    toast: Option<Toast>,
    toast_generation: u64,
    focus: FocusHandle,
    /// The right-click menu, if open.
    menu: Option<menu::Menu>,
    menu_focus: FocusHandle,
    outlook: outlook::Status,
    outlook_load: Option<Task<()>>,
    /// Files and Keychain. Absent in tests, which never touch disk or the network.
    store: Option<Store>,
    settings: Settings,
    jira_identity: Option<JiraIdentity>,
    toggl: backfill_toggl::Cache,
    statuses: Statuses,
    /// When each week was last pulled from Jira.
    pulled: HashMap<Date, std::time::Instant>,
    last_refresh: Option<std::time::Instant>,
    /// API tokens, read from the Keychain at start and after saving settings.
    jira_token: Option<String>,
    toggl_token: Option<String>,
    /// Keys found in titles that Jira says don't exist, so they aren't asked about again.
    unknown_keys: std::collections::HashSet<IssueKey>,
    /// Something changed that should be written to disk after this frame.
    dirty: bool,
    jira_task: Option<Task<()>>,
    toggl_task: Option<Task<()>>,
    git_task: Option<Task<()>>,
    search_task: Option<Task<()>>,
    /// The Settings window, while it's open.
    settings_window: Option<(WindowHandle<SettingsView>, Entity<SettingsView>, Subscription)>,
    /// The header's sync status: the popover with each connection, while open, how many
    /// have opened, and where the item was drawn, to hang the popover under it.
    status_popover: Option<status::Popover>,
    status_opened: usize,
    status_bounds: Option<gpui::Bounds<Pixels>>,
    /// When Jira was last read.
    synced_at: Option<Timestamp>,
    update: update::Update,
    /// Velopack's updater, when backfill was installed with one.
    updater: Option<velopack::UpdateManager>,
    update_task: Option<Task<()>>,
    update_checked: Option<std::time::Instant>,
    pending_update: update::Pending,
    _clock: Task<()>,
    _script: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

const SCROLL_SPRING: gpui::SpringConfig = spring(260., 30., 1.);

/// The calendar, for the menu bar, whose items act whichever window is in front.
struct Main(WeakEntity<Backfill>);

impl Global for Main {}

/// Opens Settings, at `pane` if given, from the menu bar or a shortcut in any window.
pub fn open_settings(pane: Option<Pane>, cx: &mut App) {
    if let Some(main) = cx.try_global::<Main>().and_then(|m| m.0.upgrade()) {
        main.update(cx, |this, cx| this.open_settings(pane, cx));
    }
}

/// The first time, before Jira is connected, Settings opens at Jira. Call it once the
/// calendar's window is up, so Settings opens in front of it.
pub fn open_settings_until_connected(cx: &mut App) {
    if let Some(main) = cx.try_global::<Main>().and_then(|m| m.0.upgrade()) {
        main.update(cx, |this, cx| {
            if !this.jira_configured() {
                this.open_settings(Some(Pane::Jira), cx);
            }
        });
    }
}

/// Looks for a newer version now, and shows Updates in Settings to say what came of it.
pub fn check_for_updates(cx: &mut App) {
    if let Some(main) = cx.try_global::<Main>().and_then(|m| m.0.upgrade()) {
        main.update(cx, |this, cx| {
            this.open_settings(Some(Pane::Updates), cx);
            this.check_for_updates(cx);
        });
    }
}

impl Backfill {
    /// Opens on what was saved last time, then refreshes everything in the background.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let today = Zoned::now().date();
        let store = Store::open();
        let settings: Settings = store.load("settings.json").unwrap_or_default();
        let mut ws = Workspace::new(TimeZone::system(), settings.workday());
        if let Some(saved) = store.load("state.json") {
            ws.restore(saved);
        }
        let toggl = store.load("toggl.json").unwrap_or_default();
        let mut this = Self::with_workspace(ws, today, window, cx);
        this.jira_token = store.secret(Secret::JiraToken);
        this.toggl_token = store.secret(Secret::TogglToken);
        this.store = Some(store);
        this.settings = settings;
        this.toggl = toggl;
        this.apply_toggl();
        crate::theme::assign_issue_colors(this.ws.issues.iter().map(|i| &i.key));
        this.refresh_outlook(true, cx);
        this.refresh_all(false, cx);
        this.start_updates(cx);
        cx.set_global(Main(cx.weak_entity()));
        // Settings closes with the calendar, so closing the calendar still quits backfill.
        cx.on_release(|this, cx| {
            if let Some((handle, ..)) = this.settings_window.take() {
                handle.update(cx, |_, window, _| window.remove_window()).ok();
            }
        })
        .detach();

        // Pick up changes made elsewhere while backfill was in the background.
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.set_today(Zoned::now().date(), cx);
                this.refresh_outlook(false, cx);
                let stale = this.last_refresh.is_none_or(|at| at.elapsed() > Duration::from_secs(5 * 60));
                if stale {
                    this.refresh_all(false, cx);
                }
                this.recheck_updates(cx);
            }
        });
        this._subscriptions.push(activation);
        this
    }

    pub fn with_workspace(ws: Workspace, today: Date, window: &mut Window, cx: &mut Context<Self>) -> Self {
        crate::theme::assign_issue_colors(ws.issues.iter().map(|i| &i.key));
        let week = initial_week(&ws, today);
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // Keeps the "now" line moving, moves today on at midnight, and rereads the calendar
        // every couple of minutes.
        let clock = cx.spawn(async move |this, cx| {
            for tick in 1_u64.. {
                cx.background_executor().timer(Duration::from_secs(30)).await;
                let alive = this.update(cx, |this, cx| {
                    // Tests keep the day they were given.
                    if this.store.is_some() {
                        this.set_today(Zoned::now().date(), cx);
                    }
                    if tick % 4 == 0 && matches!(this.outlook, outlook::Status::Connected { .. }) {
                        this.refresh_outlook(false, cx);
                    }
                    // A running Toggl timer grows with the clock.
                    if this.toggl.entries.values().any(|e| e.stop.is_none()) {
                        this.apply_toggl();
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            ws,
            today,
            week,
            selected: None,
            drafts: Vec::new(),
            picker: None,
            sheet: None,
            drag: None,
            blocks: Blocks::new(),
            rendered: Vec::new(),
            layout: None,
            scroll: Animated::new(FOCUS_START, SCROLL_SPRING),
            scroll_shown: FOCUS_START,
            entering_week: true,
            history: Vec::new(),
            toast: None,
            toast_generation: 0,
            focus,
            menu: None,
            menu_focus: cx.focus_handle(),
            outlook: outlook::Status::Starting,
            outlook_load: None,
            store: None,
            settings: Settings::default(),
            jira_identity: None,
            toggl: backfill_toggl::Cache::default(),
            statuses: Statuses::default(),
            pulled: HashMap::new(),
            last_refresh: None,
            jira_token: None,
            toggl_token: None,
            unknown_keys: std::collections::HashSet::new(),
            dirty: false,
            jira_task: None,
            toggl_task: None,
            git_task: None,
            search_task: None,
            settings_window: None,
            status_popover: None,
            status_opened: 0,
            status_bounds: None,
            synced_at: None,
            update: update::Update::default(),
            updater: None,
            update_task: None,
            update_checked: None,
            pending_update: update::Pending::default(),
            _clock: clock,
            _script: script::start(window, cx),
            _subscriptions: Vec::new(),
        }
    }

    /// Reloads Outlook meetings for the visible week and the weeks either side. With `ask`,
    /// macOS asks for calendar access if it hasn't yet.
    fn refresh_outlook(&mut self, ask: bool, cx: &mut Context<Self>) {
        let start = time::at(add_days(self.week, -7), 0, 0, &self.ws.tz);
        let end = time::at(add_days(self.week, 14), 0, 0, &self.ws.tz);
        let window = TimeRange::new(start, end);
        self.outlook_load = Some(cx.spawn(async move |this, cx| {
            let load = cx.background_spawn(async move { outlook::load(window, ask) }).await;
            this.update(cx, |this, cx| {
                if let Some(activities) = load.activities {
                    this.ws.replace_activities(Source::Outlook, load.window, activities);
                    this.ws.replace_skipped(load.window, load.skipped);
                }
                this.outlook = load.status;
                this.push_statuses(cx);
            })
            .ok();
        }));
    }

    fn jira_account(&self) -> Option<Account> {
        let token = self.jira_token.clone()?;
        self.settings.jira_ready().then(|| Account {
            site: self.settings.jira_site.clone(),
            email: self.settings.jira_email.clone(),
            token,
        })
    }

    fn jira_configured(&self) -> bool {
        self.jira_account().is_some()
    }

    /// The span read from Jira and git around the visible week: the week before it, the
    /// week itself and the week after.
    fn window(&self) -> TimeRange {
        TimeRange::new(time::at(add_days(self.week, -7), 0, 0, &self.ws.tz), time::at(add_days(self.week, 14), 0, 0, &self.ws.tz))
    }

    fn persist(&mut self) {
        if let Some(store) = &self.store {
            store.save("state.json", &self.ws.save());
        }
        self.dirty = false;
    }

    /// Refreshes Jira, Toggl and git. Each one skips itself when it's fresh enough, unless
    /// `asked`.
    fn refresh_all(&mut self, asked: bool, cx: &mut Context<Self>) {
        self.last_refresh = Some(std::time::Instant::now());
        self.pull_jira(asked, cx);
        self.sync_toggl(asked, cx);
        self.read_git(cx);
    }

    fn sync_now(&mut self, _: &SyncNow, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh_outlook(false, cx);
        self.refresh_all(true, cx);
        self.show_toast("Syncing Jira, Toggl, git and Outlook", false, cx);
    }

    /// Shows the latest statuses in the header and, if open, in settings.
    fn push_statuses(&mut self, cx: &mut Context<Self>) {
        self.statuses.calendar = match &self.outlook {
            outlook::Status::Starting => Status::Working("Reading the calendar".into()),
            outlook::Status::Unsupported => Status::Off,
            outlook::Status::NotBundled => Status::Problem("macOS only lets apps read the calendar. Start backfill with cargo run.".into()),
            outlook::Status::NotAsked => Status::Problem("macOS hasn't asked you yet. Press Allow access above the calendar.".into()),
            outlook::Status::Denied => Status::Problem("backfill isn't allowed to read your calendar. Allow it under Calendars in Privacy & Security.".into()),
            outlook::Status::Restricted => Status::Problem("This Mac's settings don't let apps read the calendar".into()),
            outlook::Status::Connected { calendars } if calendars.is_empty() => Status::Problem("The Calendar app has no calendars to read".into()),
            outlook::Status::Connected { calendars } => Status::Ok(format!("Reading {}", calendars.join(", ")).into()),
        };
        self.statuses.calendar_fix = self.outlook.fix();
        self.statuses.update = self.update_status();
        self.statuses.update_ready = matches!(self.update, update::Update::Ready(_));
        self.statuses.updatable = self.updater.is_some();
        if let Some((_, view, _)) = &self.settings_window {
            let mut statuses = self.statuses.clone();
            for issue in &self.ws.issues {
                let project = issue.key.project().to_owned();
                if !statuses.projects.contains(&project) {
                    statuses.projects.push(project);
                }
            }
            view.update(cx, |view, cx| view.set_statuses(statuses, cx));
        }
        cx.notify();
    }

    /// Work for a background thread: who you are in Jira (if not known yet), then your
    /// worklogs and likely issues for `window`.
    fn jira_job(
        &self,
        account: Account,
        window: TimeRange,
    ) -> impl FnOnce() -> Result<(JiraIdentity, sources::JiraPull), backfill_jira::Error> + Send + 'static {
        let identity = self.jira_identity.clone();
        let field_names = (self.settings.business_unit_field.clone(), self.settings.shared_units_field.clone());
        let tz = self.ws.tz.clone();
        let known: Vec<IssueKey> = self.ws.issues.iter().map(|i| i.key.clone()).collect();
        let mut mentioned: Vec<IssueKey> = Vec::new();
        for activity in self.ws.activities.iter().filter(|a| window.overlaps(&a.range)) {
            for key in find_issue_keys(&activity.title) {
                if !known.contains(&key) && !self.unknown_keys.contains(&key) && !mentioned.contains(&key) {
                    mentioned.push(key);
                }
            }
        }
        move || {
            let identity = match identity {
                Some(identity) => identity,
                None => sources::jira_identity(&account, &field_names)?,
            };
            let pull = sources::pull_jira(&account, &identity, window, &tz, &known, &mentioned)?;
            let found: Vec<IssueKey> = pull.issues.iter().map(|i| i.key.clone()).collect();
            let unknown = mentioned.into_iter().filter(|k| !found.contains(k)).collect();
            Ok((identity, sources::JiraPull { unknown, ..pull }))
        }
    }

    /// Takes in a Jira pull. Returns whether it worked.
    fn apply_jira(&mut self, result: Result<(JiraIdentity, sources::JiraPull), backfill_jira::Error>, week: Option<Date>, cx: &mut Context<Self>) -> bool {
        match result {
            Ok((identity, pull)) => {
                let name = identity.me.name.clone();
                self.jira_identity = Some(identity);
                self.unknown_keys.extend(pull.unknown);
                self.ws.merge_pull(pull.window, pull.worklogs);
                for (week, assigned, moved) in pull.history {
                    self.ws.history.record(week, assigned, moved);
                }
                crate::theme::assign_issue_colors(pull.issues.iter().map(|i| &i.key));
                self.ws.upsert_issues(pull.issues);
                if !pull.viewed.is_empty() {
                    self.ws.viewed = pull.viewed;
                }
                if let Some(week) = week {
                    self.pulled.insert(week, std::time::Instant::now());
                }
                self.synced_at = Some(Timestamp::now());
                let at = self.fmt_time(Timestamp::now());
                self.statuses.jira = Status::Ok(format!("Connected as {name}. Last synced at {at}.").into());
                self.persist();
                self.push_statuses(cx);
                true
            }
            Err(error) => {
                if error == backfill_jira::Error::BadCredentials {
                    self.jira_identity = None;
                }
                self.statuses.jira = Status::Problem(error.to_string().into());
                self.push_statuses(cx);
                false
            }
        }
    }

    /// Pulls the weeks around the visible one from Jira, unless that was done in the last
    /// ten minutes and nobody asked.
    fn pull_jira(&mut self, asked: bool, cx: &mut Context<Self>) {
        if self.store.is_none() {
            return;
        }
        let Some(account) = self.jira_account() else {
            self.statuses.jira = if self.settings.jira_ready() {
                Status::Problem("Add your Jira API token in Settings".into())
            } else {
                Status::Off
            };
            self.push_statuses(cx);
            return;
        };
        let week = self.week;
        let fresh = self.pulled.get(&week).is_some_and(|at| at.elapsed() < Duration::from_secs(10 * 60));
        if fresh && !asked {
            return;
        }
        if !matches!(self.statuses.jira, Status::Ok(_)) {
            self.statuses.jira = Status::Working("Connecting to Jira".into());
            self.push_statuses(cx);
        }
        let job = self.jira_job(account, self.window());
        self.jira_task = Some(cx.spawn(async move |this, cx| {
            let result = cx.background_spawn(async move { job() }).await;
            this.update(cx, |this, cx| {
                this.apply_jira(result, Some(week), cx);
            })
            .ok();
        }));
    }

    /// Toggl activities from the cache, which needs no request.
    fn apply_toggl(&mut self) {
        let now = Timestamp::now();
        self.ws.replace_activities(Source::Toggl, sources::toggl_window(now), sources::toggl_activities(&self.toggl, now));
    }

    fn sync_toggl(&mut self, asked: bool, cx: &mut Context<Self>) {
        if self.store.is_none() {
            return;
        }
        let Some(token) = self.toggl_token.clone() else {
            self.statuses.toggl = Status::Off;
            self.push_statuses(cx);
            return;
        };
        let cache = self.toggl.clone();
        let tz = self.ws.tz.clone();
        self.toggl_task = Some(cx.spawn(async move |this, cx| {
            let (cache, outcome) = cx.background_spawn(async move { sources::sync_toggl(&token, cache, &tz, asked) }).await;
            this.update(cx, |this, cx| {
                this.toggl = cache;
                if let Some(store) = &this.store {
                    store.save("toggl.json", &this.toggl);
                }
                this.apply_toggl();
                let now = Timestamp::now();
                let left = this.toggl.requests_left(now);
                let synced = this.toggl.synced_at.map(|at| this.fmt_time(at)).unwrap_or_else(|| "never".into());
                let status = match outcome {
                    backfill_toggl::Sync::Synced { .. } => Status::Ok(format!("Up to date. {left} of 24 requests left this hour.").into()),
                    backfill_toggl::Sync::Fresh => match &this.statuses.toggl {
                        Status::Ok(text) => Status::Ok(text.clone()),
                        _ => Status::Ok(format!("Synced at {synced}.").into()),
                    },
                    backfill_toggl::Sync::SavingRequests { until } => Status::Ok(
                        format!("Synced at {synced}. Waiting until {} to stay under Toggl's hourly limit.", this.fmt_time(until)).into(),
                    ),
                    backfill_toggl::Sync::Paused { until } => {
                        Status::Problem(format!("Toggl asked to slow down. Trying again after {}.", this.fmt_time(until)).into())
                    }
                    backfill_toggl::Sync::Failed(error) => Status::Problem(error.to_string().into()),
                };
                this.statuses.toggl = status;
                this.push_statuses(cx);
            })
            .ok();
        }));
    }

    fn read_git(&mut self, cx: &mut Context<Self>) {
        if self.store.is_none() {
            return;
        }
        let folders = self.settings.folders();
        if folders.is_empty() {
            self.statuses.git = Status::Off;
            self.push_statuses(cx);
            return;
        }
        let settings = self.settings.clone();
        let window = self.window();
        let tz = self.ws.tz.clone();
        self.git_task = Some(cx.spawn(async move |this, cx| {
            let (activities, repos) =
                cx.background_spawn(async move { sources::git_activities(&folders, |repo| settings.tracks(repo), window, &tz) }).await;
            this.update(cx, |this, cx| {
                let moments = activities.len();
                let (branches, commits) = activities.into_iter().partition(|a| a.source == Source::Branch);
                this.ws.replace_activities(Source::Git, window, commits);
                this.ws.replace_activities(Source::Branch, window, branches);
                this.statuses.git = if repos == 0 {
                    Status::Problem("No repositories to read. Pick some under Code.".into())
                } else {
                    Status::Ok(format!("Reading {repos} repositories: {moments} commits and branch switches around this week name an issue.").into())
                };
                this.push_statuses(cx);
            })
            .ok();
        }));
    }

    /// Looks the search text up in Jira, a moment after typing stops. The picker says
    /// it's searching, then what came of it.
    fn search_jira(&mut self, query: String, cx: &mut Context<Self>) {
        let Some(picker) = self.picker.as_ref().map(|p| p.entity.clone()) else { return };
        let mut projects: Vec<String> = Vec::new();
        for issue in &self.ws.issues {
            let project = issue.key.project().to_owned();
            if !projects.contains(&project) {
                projects.push(project);
            }
        }
        let queries = sources::search_queries(&query, &projects, &self.ws.settings.projects);
        let (Some(account), Some(identity)) = (self.jira_account(), self.jira_identity.clone()) else { return };
        if queries.is_empty() {
            self.search_task = None;
            picker.update(cx, |picker, cx| picker.set_search(Search::Idle, cx));
            return;
        }
        picker.update(cx, |picker, cx| picker.set_search(Search::Running(query.clone()), cx));
        self.search_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(250)).await;
            let found = cx.background_spawn(async move { sources::search_jira(&account, &identity, &queries) }).await;
            this.update(cx, |this, cx| {
                let rows = found.ok().map(|found| {
                    crate::theme::assign_issue_colors(found.iter().map(|i| &i.key));
                    this.ws.upsert_issues(found.clone());
                    let row = |issue: backfill_core::Issue| Row {
                        outside: !this.ws.settings.allows(&issue.key),
                        issue,
                        reasons: Vec::new(),
                        found_for: Some(query.clone()),
                    };
                    found.into_iter().map(row).collect()
                });
                picker.update(cx, |picker, cx| picker.search_finished(query, rows, cx));
            })
            .ok();
        }));
    }

    /// Brings the Settings window to the front, at `pane` if given, opening it if need be.
    pub fn open_settings(&mut self, pane: Option<Pane>, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else { return };
        if let Some((handle, ..)) = &self.settings_window {
            let shown = handle.update(cx, |view, window, cx| {
                if let Some(pane) = pane {
                    view.show(pane, window, cx);
                }
                window.activate();
            });
            if shown.is_ok() {
                return;
            }
        }
        let (settings, statuses) = (self.settings.clone(), self.statuses.clone());
        let pane = pane.unwrap_or(Pane::Jira);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(settings::WINDOW_W), px(settings::WINDOW_H)), cx))),
            titlebar: Some(TitlebarOptions {
                title: Some("Settings".into()),
                appears_transparent: true,
                traffic_light_position: Some(point(px(18.), px(settings::TOP_H / 2. - 7.))),
            }),
            app_owns_titlebar_drag: true,
            window_min_size: Some(size(px(640.), px(440.))),
            is_minimizable: false,
            ..Default::default()
        };
        let opened = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| SettingsView::new(store, settings, statuses, pane, window, cx));
            view.update(cx, |view, cx| view.focus_first_field(window, cx));
            view
        });
        let Some((handle, view)) = opened.ok().and_then(|handle| Some((handle, handle.entity(cx).ok()?))) else { return };
        let subscription = cx.subscribe(&view, Self::settings_changed);
        self.settings_window = Some((handle, view, subscription));
        self.push_statuses(cx);
    }

    /// Applies a change made in Settings, reconnecting only what it touched.
    fn settings_changed(&mut self, view: Entity<SettingsView>, event: &SettingsEvent, cx: &mut Context<Self>) {
        self.settings = view.read(cx).settings().clone();
        self.ws.settings = self.settings.workday();
        let Some(store) = self.store.clone() else { return };
        match event {
            SettingsEvent::JiraChanged => {
                self.jira_token = store.secret(Secret::JiraToken);
                self.jira_identity = None;
                self.pulled.clear();
                self.unknown_keys.clear();
                self.statuses.jira = Status::Off;
                self.pull_jira(true, cx);
            }
            SettingsEvent::TogglChanged => {
                self.toggl_token = store.secret(Secret::TogglToken);
                if self.toggl_token.is_some() {
                    self.statuses.toggl = Status::Working("Connecting to Toggl".into());
                } else {
                    // Disconnected: its timers go too.
                    self.toggl = backfill_toggl::Cache::default();
                    store.save("toggl.json", &self.toggl);
                    self.apply_toggl();
                }
                self.sync_toggl(true, cx);
            }
            SettingsEvent::CodeChanged => {
                self.git_task = None;
                self.read_git(cx);
            }
            SettingsEvent::WorkChanged => {}
            SettingsEvent::AskForCalendar => self.refresh_outlook(true, cx),
            SettingsEvent::CheckForUpdatesChanged(on) => self.set_check_for_updates(*on, cx),
            SettingsEvent::CheckForUpdates => self.check_for_updates(cx),
            SettingsEvent::RestartToUpdate => self.restart_to_update(cx),
        }
        self.push_statuses(cx);
    }

    pub(crate) fn open_picker_entity(&self) -> Option<Entity<IssuePicker>> {
        self.picker.as_ref().map(|p| p.entity.clone())
    }

    fn date(&self, day: usize) -> Date {
        add_days(self.week, day as i64)
    }

    fn day_index(&self, date: Date) -> Option<usize> {
        let offset = (date - self.week).get_days();
        (0..DAYS as i32).contains(&offset).then_some(offset as usize)
    }

    fn minutes(&self, ts: Timestamp, day: usize) -> f32 {
        time::minutes_into_day(ts, self.date(day), &self.ws.tz)
    }

    fn at_minutes(&self, minutes: f32, day: usize) -> Timestamp {
        time::from_minutes_into_day(minutes, self.date(day), &self.ws.tz)
    }

    fn fmt_time(&self, ts: Timestamp) -> String {
        ts.to_zoned(self.ws.tz.clone()).strftime("%H:%M").to_string()
    }

    fn fmt_range(&self, range: &TimeRange) -> String {
        format!("{}–{}", self.fmt_time(range.start), self.fmt_time(range.end))
    }

    fn has_happened(&self, date: Date) -> bool {
        date <= self.today
    }

    /// Remembers the state before a change, for undo. Every change goes through here, so
    /// it also marks the state to be saved.
    fn checkpoint(&mut self) {
        self.dirty = true;
        self.history.push(Snapshot { worklogs: self.ws.worklogs.clone(), activities: self.ws.activities.clone() });
        if self.history.len() > 100 {
            self.history.remove(0);
        }
    }

    fn show_toast(&mut self, message: impl Into<SharedString>, undo: bool, cx: &mut Context<Self>) {
        self.toast_generation += 1;
        let generation = self.toast_generation;
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(4500)).await;
            this.update(cx, |this, cx| {
                if this.toast.as_ref().is_some_and(|t| t.generation == generation) {
                    this.toast = None;
                    cx.notify();
                }
            })
            .ok();
        });
        self.toast = Some(Toast { message: message.into(), undo, generation, _task: task });
        cx.notify();
    }

    /// Moves today on when the date changes while backfill is open, and the view with it
    /// when it was showing this week.
    fn set_today(&mut self, today: Date, cx: &mut Context<Self>) {
        if today == self.today {
            return;
        }
        let was_this_week = self.week == week_start(self.today);
        self.today = today;
        if was_this_week {
            self.set_week(week_start(today), cx);
        }
        cx.notify();
    }

    fn set_week(&mut self, week: Date, cx: &mut Context<Self>) {
        if week != self.week {
            self.week = week;
            self.selected = None;
            self.menu = None;
            self.entering_week = true;
            if matches!(self.outlook, outlook::Status::Connected { .. }) {
                self.refresh_outlook(false, cx);
            }
            self.pull_jira(false, cx);
            self.read_git(cx);
            cx.notify();
        }
    }

    /// The day an action without an explicit target applies to: the selection's day, or
    /// the first day this week that doesn't add up yet.
    fn focus_day(&self) -> Date {
        let from_selection = match self.selected {
            Some(Selected::Day(date)) => Some(date),
            Some(Selected::Worklog(id)) => self.ws.worklog(id).map(|w| local_date(w.range.start, &self.ws.tz)),
            Some(Selected::Activity(id)) => self.ws.activity(id).map(|a| local_date(a.range.start, &self.ws.tz)),
            None => None,
        };
        from_selection.unwrap_or_else(|| {
            let unfinished = (0..DAYS)
                .map(|d| self.date(d))
                .take_while(|date| self.has_happened(*date))
                .find(|date| !self.ws.fill_plan(*date).is_empty());
            unfinished.unwrap_or(if self.day_index(self.today).is_some() { self.today } else { self.week })
        })
    }

    // Actions

    pub(crate) fn link(&mut self, _: &Link, window: &mut Window, cx: &mut Context<Self>) {
        let target = match self.selected {
            Some(Selected::Activity(id)) => PickTarget::Activity(id),
            Some(Selected::Worklog(id)) => PickTarget::Worklog(id),
            Some(Selected::Day(date)) => return self.fill(date, window, cx),
            None => {
                self.show_toast("Select a meeting, a block of time or a day first", false, cx);
                return;
            }
        };
        self.open_picker(target, window, cx);
    }

    /// Drafts the rest of a day and asks which issue it goes on. The drafts can be moved
    /// and resized first.
    pub(crate) fn fill_day(&mut self, _: &FillDay, window: &mut Window, cx: &mut Context<Self>) {
        let date = self.focus_day();
        self.fill(date, window, cx);
    }

    pub(super) fn fill(&mut self, date: Date, window: &mut Window, cx: &mut Context<Self>) {
        let plan = self.ws.fill_plan(date);
        if plan.is_empty() {
            self.show_toast(format!("{} already adds up to {}", day_name(date), format_hours(self.ws.settings.target)), false, cx);
            return;
        }
        if plan.gaps.is_empty() {
            // Only meetings that name their issue are left: nothing to ask.
            self.checkpoint();
            let logged = self.ws.log_named_meetings(date).len();
            let meetings = if logged == 1 { "1 meeting".to_owned() } else { format!("{logged} meetings") };
            self.show_toast(format!("Logged {meetings} on the issues they name"), true, cx);
            return;
        }
        self.selected = Some(Selected::Day(date));
        self.drafts = plan.gaps;
        if let Some(first) = self.drafts.first().copied() {
            let day = self.day_index(date).unwrap_or(0);
            self.scroll_into_view(self.minutes(first.start, day));
        }
        self.open_picker(PickTarget::Drafts { fill: Some(date) }, window, cx);
    }

    pub(crate) fn toggle_absorb(&mut self, _: &ToggleAbsorb, _: &mut Window, cx: &mut Context<Self>) {
        let Some(Selected::Activity(id)) = self.selected else {
            self.show_toast("Select a meeting first", false, cx);
            return;
        };
        let Some(activity) = self.ws.activity(id).cloned() else { return };
        if activity.source != Source::Outlook {
            return;
        }
        self.checkpoint();
        let absorb = !activity.absorbed;
        let changed = self.ws.set_absorbed(id, absorb).len();
        let message = match (absorb, changed) {
            (true, 1) => format!("“{}” is now part of the main task", activity.title),
            (true, n) => format!("All {n} “{}” meetings are now part of the main task", activity.title),
            (false, 1) => format!("“{}” will be logged on its own", activity.title),
            (false, n) => format!("All {n} “{}” meetings will be logged on their own", activity.title),
        };
        self.show_toast(message, true, cx);
    }

    pub(crate) fn delete_selection(&mut self, _: &DeleteSelection, _: &mut Window, cx: &mut Context<Self>) {
        let Some(Selected::Worklog(id)) = self.selected else { return };
        self.checkpoint();
        if let Some(removed) = self.ws.delete_worklog(id) {
            let note = if removed.remote_id.is_some() { ". Jira keeps it until you sync" } else { "" };
            self.show_toast(format!("Removed {} on {}{note}", format_hours(removed.range.duration()), removed.issue), true, cx);
        }
        self.selected = None;
    }

    pub(crate) fn prev_week(&mut self, _: &PrevWeek, _: &mut Window, cx: &mut Context<Self>) {
        self.set_week(add_days(self.week, -7), cx);
    }

    pub(crate) fn next_week(&mut self, _: &NextWeek, _: &mut Window, cx: &mut Context<Self>) {
        self.set_week(add_days(self.week, 7), cx);
    }

    fn this_week(&mut self, _: &ThisWeek, _: &mut Window, cx: &mut Context<Self>) {
        self.set_week(week_start(self.today), cx);
    }

    /// Selects the next thing that needs a decision, day by day. A meeting that names no
    /// issue comes first, since filling the day would fold it into the main task. Then
    /// a day with time still to log.
    pub(crate) fn next_attention(&mut self, _: &NextAttention, _: &mut Window, cx: &mut Context<Self>) {
        for day in 0..DAYS {
            let date = self.date(day);
            if !self.has_happened(date) {
                break;
            }
            let covered = |a: &Activity| self.ws.worklogs_on(date).any(|w| w.range.start <= a.range.start && w.range.end >= a.range.end);
            let mut open: Vec<&Activity> = self
                .ws
                .activities_on(date)
                .filter(|a| a.source == Source::Outlook && !a.absorbed && self.ws.issue_hint(a).is_none())
                .filter(|a| self.ws.worklog_for(a.id).is_none() && !covered(a))
                .collect();
            open.sort_by_key(|a| a.range.start);
            if let Some(activity) = open.first() {
                let (id, start) = (activity.id, self.minutes(activity.range.start, day));
                self.selected = Some(Selected::Activity(id));
                self.scroll_into_view(start);
                cx.notify();
                return;
            }
            if !self.ws.fill_plan(date).is_empty() {
                self.selected = Some(Selected::Day(date));
                cx.notify();
                return;
            }
        }
        self.show_toast("Nothing left to log this week", false, cx);
    }

    /// Opens the review of what a sync would change. With Jira connected, it first pulls
    /// the affected days again, so changes made in Jira meanwhile show up in the review
    /// instead of being overwritten unseen.
    pub(crate) fn review_sync(&mut self, _: &ReviewSync, window: &mut Window, cx: &mut Context<Self>) {
        let ops = self.ws.sync_plan();
        if ops.is_empty() {
            self.show_toast("Everything here matches Jira", false, cx);
            return;
        }
        self.picker = None;
        if self.store.is_none() {
            // Tests and screenshots: there's no Jira to ask.
            self.open_sheet(Check::Done, window, cx);
            return;
        }
        if self.jira_account().is_none() {
            self.show_toast("Connect Jira in Settings to sync", false, cx);
            self.open_settings(Some(Pane::Jira), cx);
            return;
        }
        // The review opens at once and shows the check; Sync waits for it.
        self.open_sheet(Check::Running, window, cx);
        self.check_jira(window, cx);
    }

    /// Reads the days the pending changes touch from Jira again, then refreshes the review.
    fn check_jira(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ops = self.ws.sync_plan();
        let Some(account) = self.jira_account() else { return };
        if let Some(sheet) = self.sheet.as_mut() {
            sheet.check = Check::Running;
        }
        let starts = ops.iter().map(|op| match op {
            SyncOp::Create(w) | SyncOp::Update { after: w, .. } | SyncOp::Move { after: w, .. } => w.range.start,
            SyncOp::Delete(r) => r.range.start,
        });
        let (first, last) = starts.fold((None::<Timestamp>, None::<Timestamp>), |(lo, hi), t| {
            (Some(lo.map_or(t, |lo| lo.min(t))), Some(hi.map_or(t, |hi| hi.max(t))))
        });
        let (Some(first), Some(last)) = (first, last) else { return };
        let day = |ts: Timestamp, offset: i64| time::at(add_days(local_date(ts, &self.ws.tz), offset), 0, 0, &self.ws.tz);
        let span = TimeRange::new(day(first, 0), day(last, 1));
        let job = self.jira_job(account, span);
        self.jira_task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx.background_spawn(async move { job() }).await;
            this.update_in(cx, |this, window, cx| {
                let failure = result.as_ref().err().map(ToString::to_string);
                let outcome = if this.apply_jira(result, None, cx) { Ok(()) } else { Err(failure.unwrap_or_default()) };
                this.finish_check(outcome, window, cx);
            })
            .ok();
        }));
        cx.notify();
    }

    /// Takes in the check with Jira: the review shows what's left to sync, or why it can't.
    pub(super) fn finish_check(&mut self, outcome: Result<(), String>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = self.sheet.as_mut().filter(|s| !s.running && !s.finished) else { return };
        match outcome {
            Ok(()) => {
                let ops = self.ws.sync_plan();
                if ops.is_empty() {
                    self.close_sheet(window, cx);
                    self.show_toast("Everything here matches Jira", false, cx);
                    return;
                }
                sheet.status = vec![OpStatus::Waiting; ops.len()];
                sheet.ops = ops;
                sheet.check = Check::Done;
            }
            Err(error) => sheet.check = Check::Failed(error),
        }
        cx.notify();
    }

    fn open_sheet(&mut self, check: Check, window: &mut Window, cx: &mut Context<Self>) {
        let ops = self.ws.sync_plan();
        if ops.is_empty() {
            self.show_toast("Everything here matches Jira", false, cx);
            return;
        }
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.sheet = Some(Sheet {
            status: vec![OpStatus::Waiting; ops.len()],
            ops,
            check,
            running: false,
            finished: false,
            focus,
            _task: None,
        });
        cx.notify();
    }

    /// Writes the reviewed changes to Jira one at a time. A change that fails stays waiting
    /// for the next sync, and the rest still go through.
    pub(crate) fn apply_sync(&mut self, _: &ApplySync, window: &mut Window, cx: &mut Context<Self>) {
        let account = self.jira_account();
        let live = self.store.is_some();
        let tz = self.ws.tz.clone();
        let Some(sheet) = self.sheet.as_mut() else { return };
        if sheet.finished {
            self.close_sheet(window, cx);
            return;
        }
        match sheet.check {
            Check::Done => {}
            Check::Running => return,
            Check::Failed(_) => return self.check_jira(window, cx),
        }
        if sheet.running {
            return;
        }
        sheet.running = true;
        let ops = sheet.ops.clone();
        sheet._task = Some(cx.spawn(async move |this, cx| {
            for (index, op) in ops.into_iter().enumerate() {
                let started = this.update(cx, |this, cx| {
                    if let Some(sheet) = this.sheet.as_mut() {
                        sheet.status[index] = OpStatus::Working;
                    }
                    cx.notify();
                });
                if started.is_err() {
                    return;
                }
                let result: Result<Option<backfill_core::RemoteWorklogId>, String> = match (&account, live) {
                    (Some(account), true) => {
                        let (account, op, tz) = (account.clone(), op.clone(), tz.clone());
                        cx.background_spawn(async move { sources::apply_to_jira(&account, &op, &tz).map_err(|e| e.to_string()) }).await
                    }
                    (None, true) => Err("Jira isn't connected".into()),
                    // Tests and screenshots: pretend, with made-up ids.
                    (_, false) => {
                        cx.background_executor().timer(Duration::from_millis(320)).await;
                        Ok(None)
                    }
                };
                this.update(cx, |this, cx| {
                    let status = match result {
                        Ok(created) => {
                            let needs_id = matches!(op, SyncOp::Create(_) | SyncOp::Move { .. });
                            let created = created.or_else(|| {
                                needs_id.then(|| backfill_core::RemoteWorklogId(format!("local-{}", this.ws.next_id())))
                            });
                            this.ws.record_synced(&op, created);
                            this.dirty = true;
                            OpStatus::Done
                        }
                        Err(error) => OpStatus::Failed(error),
                    };
                    if let Some(sheet) = this.sheet.as_mut() {
                        sheet.status[index] = status;
                    }
                    cx.notify();
                })
                .ok();
            }
            this.update(cx, |this, cx| {
                // Undo can't reach into Jira, so what's synced can't be undone here.
                this.history.clear();
                this.persist();
                if let Some(sheet) = this.sheet.as_mut() {
                    sheet.finished = true;
                    sheet.running = false;
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    fn close_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sheet.as_ref().is_some_and(|s| s.running) {
            return;
        }
        self.sheet = None;
        window.focus(&self.focus, cx);
        cx.notify();
    }

    /// Opens the issue in the browser: the one highlighted in the search, or the one the
    /// selected block is logged on or names.
    fn open_in_jira(&mut self, _: &OpenInJira, _: &mut Window, cx: &mut Context<Self>) {
        let key = match &self.picker {
            Some(picker) => picker.entity.read(cx).highlighted(),
            None => self.selected_issue(),
        };
        let Some(key) = key else {
            self.show_toast("Select something logged on an issue to open it in Jira", false, cx);
            return;
        };
        let site = self.settings.jira_site.trim();
        if site.is_empty() {
            self.show_toast("Add your Jira site in Settings to open issues", false, cx);
            return;
        }
        cx.open_url(&format!("https://{site}/browse/{key}"));
    }

    /// The issue the selection is logged on, or the one a meeting's title names.
    fn selected_issue(&self) -> Option<IssueKey> {
        self.selected_issue_of(self.selected?)
    }

    fn selected_issue_of(&self, target: Selected) -> Option<IssueKey> {
        match target {
            Selected::Worklog(id) => self.ws.worklog(id).map(|w| w.issue.clone()),
            Selected::Activity(id) => {
                self.ws.worklog_for(id).map(|w| w.issue.clone()).or_else(|| self.ws.activity(id)?.issue_hint())
            }
            Selected::Day(_) => None,
        }
    }

    pub(crate) fn dismiss(&mut self, _: &Dismiss, window: &mut Window, cx: &mut Context<Self>) {
        if self.status_popover.is_some() {
            self.close_status(cx);
        } else if self.menu.is_some() {
            self.close_menu(window, cx);
        } else if self.sheet.is_some() {
            self.close_sheet(window, cx);
        } else if self.picker.is_some() {
            self.close_picker(window, cx);
        } else {
            self.selected = None;
            cx.notify();
        }
    }

    pub(crate) fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        self.undo_last(cx);
    }

    fn undo_last(&mut self, cx: &mut Context<Self>) {
        let Some(snapshot) = self.history.pop() else {
            self.show_toast("Nothing to undo", false, cx);
            return;
        };
        self.ws.worklogs = snapshot.worklogs;
        self.ws.activities = snapshot.activities;
        self.dirty = true;
        self.toast = None;
        if let Some(Selected::Worklog(id)) = self.selected
            && self.ws.worklog(id).is_none()
        {
            self.selected = None;
        }
        cx.notify();
    }

    // Picker

    fn open_picker(&mut self, target: PickTarget, window: &mut Window, cx: &mut Context<Self>) {
        let workday = |date: Date| {
            let (h, m) = self.ws.settings.day_start;
            TimeRange::starting_at(time::at(date, h, m, &self.ws.tz), self.ws.settings.target)
        };
        let (focus, hint, title, verb): (TimeRange, Option<IssueKey>, String, &str) = match &target {
            PickTarget::Activity(id) => {
                let Some(a) = self.ws.activity(*id) else { return };
                let parts = self.ws.day_parts(a);
                let total: SignedDuration = parts.iter().map(|p| p.duration()).sum();
                let when = match parts.as_slice() {
                    [one] => self.fmt_range(one),
                    many => many
                        .iter()
                        .map(|p| format!("{} {}", weekday(local_date(p.start, &self.ws.tz)), self.fmt_range(p)))
                        .collect::<Vec<_>>()
                        .join(" and "),
                };
                (a.range, self.ws.issue_hint(a), format!("Log “{}”, {when} ({})", a.title, format_hours(total)), "Log time")
            }
            PickTarget::Worklog(id) => {
                let Some(w) = self.ws.worklog(*id) else { return };
                (w.range, None, format!("Move {} from {} to another issue", format_hours(w.range.duration()), w.issue), "Move here")
            }
            PickTarget::Drafts { fill } => {
                let Some(span) = self.drafts_span() else { return };
                let focus = fill.map_or(span, workday);
                (focus, None, self.drafts_title(*fill), if fill.is_some() { "Fill day" } else { "Log time" })
            }
        };
        let rows: Vec<Row> = self
            .ws
            .suggestions(focus, hint.as_ref())
            .into_iter()
            // Moving a block offers the other issues.
            .filter(|s| !matches!(&target, PickTarget::Worklog(id) if self.ws.worklog(*id).is_some_and(|w| w.issue == s.key)))
            .filter_map(|s| Some(Row { issue: self.ws.issue(&s.key)?.clone(), reasons: s.reasons, found_for: None, outside: false }))
            .collect();

        let anchor = self.anchor_for(&target, window);
        let scope = self.ws.settings.projects.clone();
        let entity = cx.new(|cx| {
            let mut picker = IssuePicker::new(title, verb, rows, cx);
            picker.set_scope(scope);
            picker
        });
        let subscription = cx.subscribe_in(&entity, window, |this, _, event: &PickerEvent, window, cx| match event {
            PickerEvent::Picked(key) => {
                if let Some(state) = this.picker.take() {
                    this.apply_pick(state.target, key.clone(), cx);
                }
                window.focus(&this.focus, cx);
            }
            PickerEvent::Dismissed => this.close_picker(window, cx),
            PickerEvent::Query(text) => this.search_jira(text.clone(), cx),
        });
        entity.focus_handle(cx).focus(window, cx);
        self.picker = Some(PickerState { entity, target, anchor, _subscription: subscription });
        cx.notify();
    }

    /// From the start of the first draft to the end of the last.
    fn drafts_span(&self) -> Option<TimeRange> {
        let start = self.drafts.iter().map(|r| r.start).min()?;
        let end = self.drafts.iter().map(|r| r.end).max()?;
        Some(TimeRange::new(start, end))
    }

    /// What the picker says it will do with the drafts, kept current as they're dragged.
    fn drafts_title(&self, fill: Option<Date>) -> String {
        let total: SignedDuration = self.drafts.iter().map(|r| r.duration()).sum();
        match (fill, self.drafts.as_slice()) {
            (None, [range]) => format!("Log {} ({})", self.fmt_range(range), format_hours(range.duration())),
            (None, _) => format!("Log {}", format_hours(total)),
            (Some(date), _) => {
                let meetings = match self.ws.named_meetings(date).len() {
                    0 => String::new(),
                    1 => ", and 1 meeting on the issue it names".to_owned(),
                    n => format!(", and {n} meetings on the issues they name"),
                };
                format!("Fill {} with {}{meetings}", day_name(date), format_hours(total))
            }
        }
    }

    /// Where the picker opens: beside what it acts on.
    fn anchor_for(&self, target: &PickTarget, window: &Window) -> Point<Pixels> {
        let Some(l) = self.layout else { return window.mouse_position() };
        let beside = |key: BlockKey| {
            self.rendered.iter().find(|(k, _)| *k == key).map(|(_, g)| point(px(g.x + g.w + 8.), px(g.y.max(l.cal_top))))
        };
        let beside_column = |date: Date, y: f32| self.day_index(date).map(|day| point(px(l.day_x(day) + l.col_w + 8.), px(y)));
        let anchor = match target {
            PickTarget::Activity(id) => beside(BlockKey::Activity(id.0)),
            PickTarget::Worklog(id) => beside(BlockKey::Worklog(id.0)),
            PickTarget::Drafts { .. } => self.drafts.first().and_then(|range| {
                let date = local_date(range.start, &self.ws.tz);
                let minutes = self.day_index(date).map_or(0., |d| self.minutes(range.start, d));
                beside_column(date, (l.cal_top + self.calendar_y(&l, minutes)).max(l.cal_top))
            }),
        };
        anchor.unwrap_or_else(|| window.mouse_position())
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.drafts.clear();
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn apply_pick(&mut self, target: PickTarget, key: IssueKey, cx: &mut Context<Self>) {
        self.checkpoint();
        let message = match target {
            PickTarget::Activity(id) => {
                let title = self.ws.activity(id).map(|a| a.title.clone()).unwrap_or_default();
                if let Some(worklog) = self.ws.link(id, key.clone()) {
                    self.selected = Some(Selected::Worklog(worklog));
                }
                format!("Logged “{title}” on {key}")
            }
            PickTarget::Worklog(id) => {
                self.ws.set_issue(id, key.clone());
                format!("Moved to {key}")
            }
            PickTarget::Drafts { fill } => {
                let drafts = std::mem::take(&mut self.drafts);
                let total: SignedDuration = drafts.iter().map(|r| r.duration()).sum();
                let meetings = fill.map_or(0, |date| self.ws.log_named_meetings(date).len());
                let logged = self.ws.log_ranges(&key, &drafts);
                self.selected = logged.last().map(|id| Selected::Worklog(*id));
                match (fill, meetings) {
                    (None, _) => format!("Logged {} on {key}", format_hours(total)),
                    (Some(date), 0) => format!("Filled {} with {} on {key}", day_name(date), format_hours(total)),
                    (Some(date), 1) => format!("Filled {}: 1 meeting on its issue, the rest on {key}", day_name(date)),
                    (Some(date), n) => format!("Filled {}: {n} meetings on their issues, the rest on {key}", day_name(date)),
                }
            }
        };
        self.show_toast(message, true, cx);
    }

    // Pointer moves and releases anywhere in the window, so drags can leave the calendar.

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            self.drag_to(event.position, cx);
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.drag.is_some() {
            self.finish_drag(window, cx);
        }
    }
}

/// The week to open on. On a Monday with last week still incomplete, that's last week:
/// it's what most people sit down to finish.
fn initial_week(ws: &Workspace, today: Date) -> Date {
    let this_week = week_start(today);
    let last_week = add_days(this_week, -7);
    let unfinished = (0..DAYS as i64).any(|d| !ws.fill_plan(add_days(last_week, d)).is_empty());
    if today == this_week && unfinished { last_week } else { this_week }
}

fn day_name(date: Date) -> String {
    format!("{} {}", weekday(date), date.day())
}

fn weekday(date: Date) -> &'static str {
    ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][date.weekday().to_monday_zero_offset() as usize]
}

fn month(date: Date) -> &'static str {
    [
        "January", "February", "March", "April", "May", "June", "July", "August", "September",
        "October", "November", "December",
    ][date.month() as usize - 1]
}

fn ratio(value: SignedDuration, of: SignedDuration) -> f32 {
    if of.is_zero() { 0. } else { value.as_secs() as f32 / of.as_secs() as f32 }
}

impl Focusable for Backfill {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Backfill {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        self.validate_menu(window);
        let viewport = window.viewport_size();
        let lanes = self.banners().iter().map(|b| b.lane + 1).max().unwrap_or(0);
        let l = Layout::new(f32::from(viewport.width), f32::from(viewport.height), lanes);
        self.layout = Some(l);
        self.blocks.reduce_motion = cx.reduce_motion();
        self.blocks.begin_frame();

        let day_heads = self.render_day_heads(&theme, l, cx);
        let calendar = self.render_calendar(&theme, l, window, cx);
        if self.blocks.end_frame() {
            window.request_animation_frame();
        }
        self.entering_week = false;
        if self.dirty {
            self.persist();
        }
        let pending = self.ws.sync_plan().len();

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::link))
            .on_action(cx.listener(Self::fill_day))
            .on_action(cx.listener(Self::toggle_absorb))
            .on_action(cx.listener(Self::delete_selection))
            .on_action(cx.listener(Self::prev_week))
            .on_action(cx.listener(Self::next_week))
            .on_action(cx.listener(Self::this_week))
            .on_action(cx.listener(Self::next_attention))
            .on_action(cx.listener(Self::review_sync))
            .on_action(cx.listener(Self::apply_sync))
            .on_action(cx.listener(Self::dismiss))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::sync_now))
            .on_action(cx.listener(Self::open_in_jira))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(theme.canvas)
            .text_color(theme.ink)
            .child(self.render_header(&theme, l, pending, cx))
            .child(day_heads)
            .child(calendar)
            .child(self.render_footer(&theme))
            .when_some(self.toast.as_ref(), |root, toast| root.child(render_toast(&theme, toast, l, cx)))
            .when_some(self.picker.as_ref(), |root, picker| {
                root.child(
                    deferred(
                        anchored().position(picker.anchor).snap_to_window_with_margin(px(12.)).child(
                            div()
                                // Drafts are edited while the picker is open, so pressing one keeps it.
                                .on_mouse_down_out(cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                                    if !this.draft_at(event.position) {
                                        this.close_picker(window, cx);
                                    }
                                }))
                                .child(picker.entity.clone()),
                        ),
                    )
                    .priority(1),
                )
            })
            .when_some(self.render_menu(&theme, cx), |root, menu| root.child(menu))
            .when_some(self.render_status_popover(&theme, cx), |root, popover| root.child(popover))
            .when(self.sheet.is_some(), |root| root.child(self.render_sheet(&theme, cx)))
    }
}

impl Backfill {
    fn render_header(&self, theme: &Theme, l: Layout, pending: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let week = self.week;
        let friday = add_days(week, 4);
        let range = if week.month() == friday.month() {
            format!("{}–{} {}", week.day(), friday.day(), month(friday))
        } else {
            format!("{} {} – {} {}", week.day(), month(week), friday.day(), month(friday))
        };
        let is_current = week == week_start(self.today);
        let logged: SignedDuration = (0..DAYS).map(|d| self.ws.logged_on(self.date(d))).sum();
        let days_due = (0..DAYS).filter(|d| self.has_happened(self.date(*d))).count() as i32;
        let due = self.ws.settings.target * days_due;
        let target_week = self.ws.settings.target * DAYS as i32;

        let nav = |id: &'static str, label: &'static str| {
            div()
                .id(id)
                .occlude()
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded_md()
                .text_color(theme.muted)
                .text_lg()
                .cursor_pointer()
                .hover(|s| s.bg(theme.line).text_color(theme.ink))
                .child(label)
        };

        div()
            .absolute()
            .top_0()
            .left_0()
            .w(px(l.width))
            .h(px(HEADER_H))
            .flex()
            .items_center()
            .gap_3()
            // Room for the traffic lights, which sit inside the header on macOS.
            .pl(px(if cfg!(target_os = "macos") { 84. } else { 16. }))
            .pr_4()
            .border_b_1()
            .border_color(theme.line)
            // The header is the titlebar on macOS: empty space and plain text move the window,
            // and a double-click does what the system setting says (zoom, fill or minimize).
            // Buttons occlude it, so clicking them never drags or zooms.
            .when(cfg!(target_os = "macos"), |d| {
                d.on_mouse_down(MouseButton::Left, |event, window, _| {
                    if event.click_count == 2 {
                        window.titlebar_double_click();
                    } else {
                        window.start_window_move();
                    }
                })
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(nav("prev-week", "‹").on_click(cx.listener(|this, _, _, cx| this.set_week(add_days(this.week, -7), cx))))
                    .child(nav("next-week", "›").on_click(cx.listener(|this, _, _, cx| this.set_week(add_days(this.week, 7), cx)))),
            )
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_2()
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(format!("Week {}", week.iso_week_date().week())))
                    .child(div().text_sm().text_color(theme.muted).child(range)),
            )
            .when(!is_current, |d| {
                d.child(
                    div()
                        .id("this-week")
                        .occlude()
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(theme.line_strong)
                        .text_xs()
                        .text_color(theme.muted)
                        .cursor_pointer()
                        .hover(|s| s.text_color(theme.ink))
                        .on_click(cx.listener(|this, _, _, cx| this.set_week(week_start(this.today), cx)))
                        .child("Back to this week"),
                )
            })
            .child(div().flex_1())
            .children(self.render_update_button(theme, cx))
            .children(self.render_status(theme, cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .w(px(170.))
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .text_xs()
                            .child(div().child(format_hours(logged)))
                            .child(div().text_color(theme.muted).child(format!("of {} logged", format_hours(target_week)))),
                    )
                    .child(gauge(theme, "week-gauge", ratio(logged, target_week), ratio(due, target_week), theme.ink)),
            )
            .child({
                // Tests have no store and pretend Jira is there.
                let connected = self.store.is_none() || self.jira_configured();
                let label: SharedString = match pending {
                    _ if !connected => "Connect Jira".into(),
                    0 => "In sync with Jira".into(),
                    1 => "Review 1 change".into(),
                    n => format!("Review {n} changes").into(),
                };
                div()
                    .id("review-sync")
                    .occlude()
                    .px_3()
                    .py_1p5()
                    .rounded_lg()
                    .text_sm()
                    .cursor_pointer()
                    .when(connected && pending > 0, |d| d.bg(theme.ink).text_color(theme.canvas).font_weight(FontWeight::MEDIUM))
                    .when(!connected || pending == 0, |d| d.text_color(theme.muted).border_1().border_color(theme.line))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if connected {
                            this.review_sync(&ReviewSync, window, cx)
                        } else {
                            this.open_settings(Some(Pane::Jira), cx)
                        }
                    }))
                    .child(label)
            })
    }

    fn render_footer(&self, theme: &Theme) -> impl IntoElement {
        use picker::hint;
        let hints: Vec<(&'static str, &'static str)> = match self.selected {
            Some(Selected::Worklog(_)) => {
                vec![("L", "Move to another issue"), ("drag", "Move or resize"), ("Delete", "Remove"), ("⌘O", "Open in Jira")]
            }
            Some(Selected::Activity(id)) => {
                let mut hints = vec![("L", "Log on an issue")];
                if self.ws.activity(id).is_some_and(|a| a.source == Source::Outlook) {
                    hints.push(("A", "Part of the main task, or not"));
                }
                if self.selected_issue().is_some() {
                    hints.push(("⌘O", "Open in Jira"));
                }
                hints
            }
            Some(Selected::Day(_)) => vec![("F", "Fill the day"), ("drag", "Log time")],
            None => vec![("N", "Next thing to log"), ("F", "Fill a day"), ("drag", "Log time"), ("←→", "Week")],
        };
        let units = {
            let mut logged: Vec<(IssueKey, SignedDuration)> = Vec::new();
            for day in 0..DAYS {
                for (issue, hours) in self.ws.logged_by_issue(self.date(day)) {
                    match logged.iter_mut().find(|(k, _)| *k == issue) {
                        Some((_, total)) => *total += hours,
                        None => logged.push((issue, hours)),
                    }
                }
            }
            attribution::by_business_unit(logged.iter().filter_map(|(k, d)| Some((self.ws.issue(k)?, *d))))
        };
        div()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(FOOTER_H))
            .flex()
            .items_center()
            .gap_5()
            .pl(px(GUTTER_W))
            .pr_4()
            .border_t_1()
            .border_color(theme.line)
            .text_xs()
            .children(hints.into_iter().map(|(key, label)| hint(theme, key, label)))
            .child(div().flex_1())
            .when(self.settings.business_units() && !units.is_empty(), |d| {
                d.child(div().text_color(theme.muted).child("Credited to"))
                    .children(units.iter().take(4).map(|(unit, hours)| {
                        div()
                            .flex()
                            .gap_1()
                            .child(div().child(unit.clone()))
                            .child(div().text_color(theme.muted).child(format!("{hours:.1}h")))
                    }))
            })
            .child(hint(theme, "⌘ Enter", "Review sync"))
    }
}

fn render_toast(theme: &Theme, toast: &Toast, l: Layout, cx: &mut Context<Backfill>) -> impl IntoElement {
    div()
        .absolute()
        .left(px(GUTTER_W))
        .w(px(l.width - GUTTER_W - RIGHT_PAD))
        .bottom(px(FOOTER_H + 18.))
        .flex()
        .justify_center()
        .child(
            div()
                .flex()
                .items_center()
                .gap_4()
                .pl_4()
                .pr_2()
                .py_2()
                .rounded_xl()
                .bg(theme.ink)
                .text_color(theme.canvas)
                .text_sm()
                .shadow_2xl()
                .occlude()
                .child(toast.message.clone())
                .when(toast.undo, |d| {
                    d.child(
                        div()
                            .id("toast-undo")
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .font_weight(FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|s| s.bg(with_alpha(theme.canvas, 0.15)))
                            .on_click(cx.listener(|this, _, _, cx| this.undo_last(cx)))
                            .child("Undo"),
                    )
                })
                .with_animation(
                    ("toast", toast.generation as usize),
                    Animation::new(millis(240)).with_easing(ease_out_quint()),
                    |d, t| d.opacity(t).mb(px(-10. * (1. - t))),
                ),
        )
}

/// A thin progress bar. `due` marks how much should be logged by now.
fn gauge(theme: &Theme, id: impl Into<gpui::ElementId>, value: f32, due: f32, color: gpui::Hsla) -> impl IntoElement {
    div()
        .relative()
        .h(px(4.))
        .w_full()
        .rounded_full()
        .bg(theme.line)
        .child(
            div()
                .id(id)
                .absolute()
                .top_0()
                .left_0()
                .h_full()
                .rounded_full()
                .bg(color)
                .w(relative(value.clamp(0., 1.)))
                .transitions(|t| t.w(millis(500)).bg(millis(300))),
        )
        .when(due > 0. && due < 1., |d| {
            d.child(div().absolute().top(px(-2.)).left(relative(due)).w(px(1.)).h(px(8.)).bg(theme.muted))
        })
}

#[cfg(test)]
mod tests;
