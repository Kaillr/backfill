//! Settings, in a window of their own: a section each for Jira, Toggl, the calendar, code,
//! the workday and updates, listed down the side with how each one is doing.
//!
//! Changes apply as they're made. Only Jira and Toggl wait for Connect, since that's when the
//! token is checked. Tokens go to the Keychain and aren't shown again.

use std::path::Path;

use backfill_git::Repo;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, Hsla, KeyBinding, MouseButton,
    PathPromptOptions, ScrollHandle, SharedString, Subscription, Task, Window, actions, div, prelude::*, px,
};
use gpui_elements::{
    editable_text::{EditableTextState, StringStorage},
    switch::{BaseSwitch, BaseSwitchThumb},
};

use crate::{
    calendar::update,
    field::field,
    store::{Secret, Settings, Store},
    theme::{Theme, with_alpha},
};

actions!(settings, [CloseSettings, Submit, NextField, PrevField]);

const CONTEXT: &str = "Settings";
pub const WINDOW_W: f32 = 780.;
pub const WINDOW_H: f32 = 580.;
const SIDEBAR_W: f32 = 196.;
/// The strip along the top that moves the window, with the traffic lights in it.
pub const TOP_H: f32 = 52.;

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("escape", CloseSettings, Some(CONTEXT)),
        KeyBinding::new("secondary-w", CloseSettings, Some(CONTEXT)),
        // After the text field's defaults, so these win inside fields.
        KeyBinding::new("enter", Submit, Some("Settings > EditableText")),
        // A single-line field hands Tab on to the window.
        KeyBinding::new("tab", NextField, Some(CONTEXT)),
        KeyBinding::new("shift-tab", PrevField, Some(CONTEXT)),
    ]);
}

/// How a connection is doing.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Status {
    #[default]
    Off,
    Working(SharedString),
    Ok(SharedString),
    Problem(SharedString),
}

impl Status {
    pub fn color(&self, theme: &Theme) -> Hsla {
        match self {
            Status::Off => theme.faint,
            Status::Working(_) => theme.focus,
            Status::Ok(_) => theme.ok,
            Status::Problem(_) => theme.danger,
        }
    }

    fn text(&self) -> Option<SharedString> {
        match self {
            Status::Off => None,
            Status::Working(text) | Status::Ok(text) | Status::Problem(text) => Some(text.clone()),
        }
    }
}

/// What can be done about calendar access, here and in the notice over the calendar.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CalendarFix {
    /// macOS hasn't asked yet: ask.
    Ask,
    /// It was refused: change it in Privacy & Security.
    OpenPrivacy,
}

/// What Settings shows that the calendar knows: how each connection is doing, and the
/// projects to pick from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Statuses {
    pub jira: Status,
    pub toggl: Status,
    pub git: Status,
    pub calendar: Status,
    pub calendar_fix: Option<CalendarFix>,
    pub update: Status,
    /// An update is downloaded and waits for a restart.
    pub update_ready: bool,
    /// backfill was installed with its updater, rather than built from source.
    pub updatable: bool,
    /// The projects of the issues backfill knows about.
    pub projects: Vec<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pane {
    Jira,
    Toggl,
    Calendar,
    Code,
    Workday,
    Updates,
}

impl Pane {
    pub const ALL: [Pane; 6] = [Pane::Jira, Pane::Toggl, Pane::Calendar, Pane::Code, Pane::Workday, Pane::Updates];

    /// The section called `name`, such as `code`, for scripts.
    pub fn named(name: &str) -> Option<Pane> {
        Pane::ALL.into_iter().find(|p| p.title().split(' ').next().is_some_and(|t| t.eq_ignore_ascii_case(name)))
    }

    fn title(self) -> &'static str {
        match self {
            Pane::Jira => "Jira",
            Pane::Toggl => "Toggl Track",
            Pane::Calendar => "Calendar",
            Pane::Code => "Code",
            Pane::Workday => "Workday",
            Pane::Updates => "Updates",
        }
    }
}

pub enum SettingsEvent {
    /// Jira's site, email or token changed: connect again.
    JiraChanged,
    TogglChanged,
    /// The folders or the repositories to read changed.
    CodeChanged,
    /// The workday or the projects time is logged on changed.
    WorkChanged,
    AskForCalendar,
    CheckForUpdatesChanged(bool),
    CheckForUpdates,
    RestartToUpdate,
}

pub struct SettingsView {
    store: Store,
    settings: Settings,
    pane: Pane,
    statuses: Statuses,
    jira_site: Entity<EditableTextState>,
    jira_email: Entity<EditableTextState>,
    jira_token: Entity<EditableTextState>,
    toggl_token: Entity<EditableTextState>,
    jira_token_saved: bool,
    toggl_token_saved: bool,
    /// Showing the fields, to connect or to change a connection.
    editing_jira: bool,
    editing_toggl: bool,
    error: Option<(Pane, SharedString)>,
    folders: Vec<Entity<EditableTextState>>,
    /// The repositories in the folders, once they've been looked for.
    repos: Option<Vec<Repo>>,
    _scan: Option<Task<()>>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// The fields on show, in Tab order.
    order: Vec<Entity<EditableTextState>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsView {}

impl Focusable for SettingsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SettingsView {
    pub fn new(store: Store, settings: Settings, statuses: Statuses, pane: Pane, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let text = |text: &str, cx: &mut Context<Self>| cx.new(|cx| EditableTextState::new(StringStorage::from(text), cx));
        let jira_token_saved = store.secret(Secret::JiraToken).is_some();
        let toggl_token_saved = store.secret(Secret::TogglToken).is_some();
        let mut this = Self {
            jira_site: text(&settings.jira_site, cx),
            jira_email: text(&settings.jira_email, cx),
            jira_token: text("", cx),
            toggl_token: text("", cx),
            editing_jira: !jira_token_saved,
            editing_toggl: !toggl_token_saved,
            jira_token_saved,
            toggl_token_saved,
            error: None,
            folders: Vec::new(),
            repos: None,
            _scan: None,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            order: Vec::new(),
            _subscriptions: Vec::new(),
            store,
            settings,
            pane,
            statuses,
        };
        let folders = this.settings.code_folders.clone();
        for folder in folders.iter().map(String::as_str).chain(folders.is_empty().then_some("")) {
            this.add_folder_field(folder, window, cx);
        }
        this.find_repos(cx);
        this
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn show(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        self.pane = pane;
        self.focus_first_field(window, cx);
        cx.notify();
    }

    /// Puts the caret in the pane's first field, if it has one waiting to be filled.
    pub fn focus_first_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let first = match self.pane {
            Pane::Jira if self.editing_jira => Some(self.jira_site.clone()),
            Pane::Toggl if self.editing_toggl => Some(self.toggl_token.clone()),
            _ => None,
        };
        match first {
            Some(field) => field.read(cx).focus_handle(cx).focus(window, cx),
            None => self.focus.focus(window, cx),
        }
    }

    pub fn set_statuses(&mut self, statuses: Statuses, cx: &mut Context<Self>) {
        if self.statuses != statuses {
            self.statuses = statuses;
            cx.notify();
        }
    }

    fn text(field: &Entity<EditableTextState>, cx: &App) -> String {
        field.read(cx).as_str().trim().to_owned()
    }

    fn save(&self) {
        self.store.save("settings.json", &self.settings);
    }

    fn select(&mut self, pane: Pane, window: &mut Window, cx: &mut Context<Self>) {
        if pane != self.pane {
            self.error = None;
            self.scroll.set_offset(gpui::point(px(0.), px(0.)));
        }
        self.show(pane, window, cx);
    }

    fn close(&mut self, _: &CloseSettings, window: &mut Window, _: &mut Context<Self>) {
        window.remove_window();
    }

    /// Enter: connect, or take the folder that was typed.
    fn submit(&mut self, _: &Submit, window: &mut Window, cx: &mut Context<Self>) {
        match self.pane {
            Pane::Jira => self.connect_jira(window, cx),
            Pane::Toggl => self.connect_toggl(window, cx),
            Pane::Code => {
                self.commit_folders(cx);
                self.focus.focus(window, cx);
            }
            _ => {}
        }
    }

    fn next_field(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(1, window, cx);
    }

    fn prev_field(&mut self, _: &PrevField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(-1, window, cx);
    }

    /// Focuses the field `step` places along, wrapping around.
    fn move_focus(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.order.len() as isize;
        if count == 0 {
            return;
        }
        let current = self.order.iter().position(|f| f.read(cx).focus_handle(cx).is_focused(window));
        let next = match current {
            Some(i) => (i as isize + step).rem_euclid(count) as usize,
            None if step > 0 => 0,
            None => count as usize - 1,
        };
        self.order[next].read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    // Jira and Toggl.

    fn connect_jira(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let site = clean_site(&Self::text(&self.jira_site, cx));
        let email = Self::text(&self.jira_email, cx);
        let token = Self::text(&self.jira_token, cx);
        if site.is_empty() || email.is_empty() || (token.is_empty() && !self.jira_token_saved) {
            self.error = Some((Pane::Jira, "Fill in your Jira site, your email and an API token".into()));
            cx.notify();
            return;
        }
        if !token.is_empty() {
            if let Err(error) = self.store.set_secret(Secret::JiraToken, &token) {
                self.error = Some((Pane::Jira, format!("Couldn't save the token in the Keychain: {error}").into()));
                cx.notify();
                return;
            }
            self.jira_token_saved = true;
            self.jira_token.update(cx, |f, cx| f.emplace("", cx));
        }
        self.jira_site.update(cx, |f, cx| f.emplace(&site, cx));
        self.settings.jira_site = site;
        self.settings.jira_email = email;
        self.save();
        self.error = None;
        self.editing_jira = false;
        self.focus.focus(window, cx);
        cx.emit(SettingsEvent::JiraChanged);
        cx.notify();
    }

    fn disconnect_jira(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.store.set_secret(Secret::JiraToken, "").ok();
        self.jira_token_saved = false;
        self.editing_jira = true;
        self.focus_first_field(window, cx);
        cx.emit(SettingsEvent::JiraChanged);
        cx.notify();
    }

    fn connect_toggl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let token = Self::text(&self.toggl_token, cx);
        if token.is_empty() {
            self.error = Some((Pane::Toggl, "Paste your Toggl API token".into()));
            cx.notify();
            return;
        }
        if let Err(error) = self.store.set_secret(Secret::TogglToken, &token) {
            self.error = Some((Pane::Toggl, format!("Couldn't save the token in the Keychain: {error}").into()));
            cx.notify();
            return;
        }
        self.toggl_token.update(cx, |f, cx| f.emplace("", cx));
        self.toggl_token_saved = true;
        self.editing_toggl = false;
        self.error = None;
        self.focus.focus(window, cx);
        cx.emit(SettingsEvent::TogglChanged);
        cx.notify();
    }

    fn disconnect_toggl(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.store.set_secret(Secret::TogglToken, "").ok();
        self.toggl_token_saved = false;
        self.editing_toggl = true;
        self.focus_first_field(window, cx);
        cx.emit(SettingsEvent::TogglChanged);
        cx.notify();
    }

    /// Picks a project to log time on, or puts it back. None picked means any.
    fn toggle_project(&mut self, project: &str, cx: &mut Context<Self>) {
        let projects = &mut self.settings.jira_projects;
        match projects.iter().position(|p| p.eq_ignore_ascii_case(project)) {
            Some(i) => {
                projects.remove(i);
            }
            None => projects.push(project.to_owned()),
        }
        self.save();
        cx.emit(SettingsEvent::WorkChanged);
        cx.notify();
    }

    // Code.

    fn add_folder_field(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<EditableTextState> {
        let field = cx.new(|cx| EditableTextState::new(StringStorage::from(text), cx));
        let focus = field.read(cx).focus_handle(cx);
        // A typed folder counts once the field is left.
        self._subscriptions.push(cx.on_blur(&focus, window, |this, _, cx| this.commit_folders(cx)));
        self.folders.push(field.clone());
        field
    }

    fn remove_folder(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.folders.len() {
            self.folders.remove(index);
            self.commit_folders(cx);
            cx.notify();
        }
    }

    /// Opens the system's folder picker and puts the folder chosen in field `index`, or in a
    /// new one.
    fn choose_folder(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let chosen = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Choose".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = chosen.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            this.update_in(cx, |this, window, cx| {
                let text = tilde(&path);
                let field = match index.and_then(|i| this.folders.get(i)) {
                    Some(field) => field.clone(),
                    None => this.add_folder_field("", window, cx),
                };
                field.update(cx, |f, cx| f.emplace(&text, cx));
                this.commit_folders(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Saves the folders from their fields, when they changed, and looks for repositories again.
    fn commit_folders(&mut self, cx: &mut Context<Self>) {
        let folders: Vec<String> = self.folders.iter().map(|f| Self::text(f, cx)).filter(|f| !f.is_empty()).collect();
        if folders == self.settings.code_folders {
            return;
        }
        self.settings.code_folders = folders;
        self.save();
        self.find_repos(cx);
        cx.emit(SettingsEvent::CodeChanged);
        cx.notify();
    }

    fn find_repos(&mut self, cx: &mut Context<Self>) {
        let folders = self.settings.folders();
        self.repos = None;
        self._scan = Some(cx.spawn(async move |this, cx| {
            let repos = cx.background_spawn(async move { backfill_git::discover(&folders, &[]) }).await;
            this.update(cx, |this, cx| {
                this.repos = Some(repos);
                cx.notify();
            })
            .ok();
        }));
    }

    /// Reads `paths`, or stops reading them. From then on the repositories ticked here decide,
    /// and the organisations older settings listed don't.
    fn set_reading(&mut self, paths: &[&Path], read: bool, cx: &mut Context<Self>) {
        let Some(repos) = &self.repos else { return };
        let mut ignored: Vec<String> = self
            .settings
            .ignored_repositories
            .iter()
            .filter(|p| !repos.iter().any(|r| r.path == Path::new(p.as_str())))
            .cloned()
            .collect();
        for repo in repos {
            let reading = if paths.contains(&repo.path.as_path()) { read } else { self.settings.tracks(repo) };
            if !reading {
                ignored.push(repo.path.to_string_lossy().into_owned());
            }
        }
        self.settings.ignored_repositories = ignored;
        self.settings.organisations.clear();
        self.save();
        cx.emit(SettingsEvent::CodeChanged);
        cx.notify();
    }

    // Workday.

    fn step_hours(&mut self, step: f64, cx: &mut Context<Self>) {
        let hours = ((self.settings.hours_per_day + step) * 4.).round() / 4.;
        self.settings.hours_per_day = hours.clamp(1., 16.);
        self.save();
        cx.emit(SettingsEvent::WorkChanged);
        cx.notify();
    }

    fn step_start(&mut self, minutes: i32, cx: &mut Context<Self>) {
        let (h, m) = self.settings.start();
        let start = (h as i32 * 60 + m as i32 + minutes).clamp(0, 23 * 60 + 45);
        self.settings.day_start = format!("{:02}:{:02}", start / 60, start % 60);
        self.save();
        cx.emit(SettingsEvent::WorkChanged);
        cx.notify();
    }

    // Updates.

    fn set_check_for_updates(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.check_for_updates = on;
        self.save();
        cx.emit(SettingsEvent::CheckForUpdatesChanged(on));
        cx.notify();
    }
}

/// `example.atlassian.net`, from however the site was pasted, such as a whole URL.
fn clean_site(text: &str) -> String {
    let text = text.trim();
    let text = text.split_once("://").map_or(text, |(_, rest)| rest);
    text.split('/').next().unwrap_or("").to_owned()
}

/// A folder as shown, with the home folder as `~`.
fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `owner/repo` from a remote such as `git@github.com:owner/repo.git`.
fn origin_name(origin: &str) -> String {
    let rest = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, rest)| rest);
    let path = rest.find([':', '/']).map_or(rest, |i| &rest[i + 1..]);
    path.trim_end_matches('/').trim_end_matches(".git").to_owned()
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        self.order.clear();
        let pane = self.pane;
        let body = match pane {
            Pane::Jira => self.render_jira(&theme, window, cx),
            Pane::Toggl => self.render_toggl(&theme, window, cx),
            Pane::Calendar => self.render_calendar(&theme, cx),
            Pane::Code => self.render_code(&theme, window, cx),
            Pane::Workday => self.render_workday(&theme, cx),
            Pane::Updates => self.render_updates(&theme, cx),
        };
        let mover = |d: gpui::Div| {
            d.on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
        };

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::next_field))
            .on_action(cx.listener(Self::prev_field))
            .size_full()
            .flex()
            .bg(theme.surface)
            .text_color(theme.ink)
            .child(
                div()
                    .w(px(SIDEBAR_W))
                    .h_full()
                    .flex()
                    .flex_col()
                    .bg(theme.canvas)
                    .border_r_1()
                    .border_color(theme.line)
                    .child(mover(div().h(px(TOP_H)).flex_shrink_0()))
                    .child(div().flex().flex_col().gap_0p5().px_2().children(Pane::ALL.map(|p| self.render_nav(p, &theme, cx)))),
            )
            .child(
                div()
                    .flex_1()
                    // Or the text sets its width, unwrapped, and runs off the window.
                    .min_w(px(0.))
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(
                        mover(div())
                            .h(px(TOP_H))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .px_6()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(pane.title()),
                    )
                    .child(
                        div()
                            .id("settings-body")
                            .track_scroll(&self.scroll)
                            .flex_1()
                            .overflow_y_scroll()
                            .px_6()
                            .pb_6()
                            .flex()
                            .flex_col()
                            .gap_5()
                            .child(body),
                    ),
            )
    }
}

// The parts each pane is made of.

fn description(theme: &Theme, text: impl Into<SharedString>) -> impl IntoElement {
    div().text_sm().text_color(theme.muted).child(text.into())
}

fn label(theme: &Theme, text: &'static str) -> impl IntoElement {
    div().text_xs().font_weight(FontWeight::MEDIUM).text_color(theme.muted).child(text)
}

fn group(theme: &Theme, title: &'static str) -> gpui::Div {
    div().flex().flex_col().gap_2().child(label(theme, title))
}

fn status_line(theme: &Theme, status: &Status) -> Option<AnyElement> {
    let text = status.text()?;
    Some(
        div()
            .flex()
            .items_start()
            .gap_2()
            .text_sm()
            .child(div().mt(px(6.)).size(px(7.)).flex_shrink_0().rounded_full().bg(status.color(theme)))
            .child(div().text_color(if matches!(status, Status::Problem(_)) { theme.danger } else { theme.ink }).child(text))
            .into_any_element(),
    )
}

fn button(theme: &Theme, id: impl Into<gpui::ElementId>, text: impl Into<SharedString>, primary: bool) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex_shrink_0()
        .px_3()
        .py_1p5()
        .rounded_md()
        .text_sm()
        .cursor_pointer()
        .when(primary, |d| d.bg(theme.ink).text_color(theme.canvas).font_weight(FontWeight::MEDIUM).hover(|s| s.opacity(0.9)))
        .when(!primary, |d| {
            d.border_1().border_color(theme.line_strong).text_color(theme.ink).hover(|s| s.bg(theme.line))
        })
        .child(text.into())
}

fn link(theme: &Theme, id: &'static str, text: &'static str, url: &'static str) -> impl IntoElement {
    div()
        .id(id)
        .text_sm()
        .text_color(theme.focus)
        .cursor_pointer()
        .hover(|s| s.underline())
        .on_click(move |_, _, cx| cx.open_url(url))
        .child(text)
}

fn error_line(theme: &Theme, error: Option<SharedString>) -> Option<AnyElement> {
    error.map(|e| div().text_sm().text_color(theme.danger).child(e).into_any_element())
}

/// A box with a tick, which `checked` fills.
fn tick(theme: &Theme, checked: bool) -> impl IntoElement {
    div()
        .size(px(16.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .text_xs()
        .when(checked, |d| d.bg(theme.focus).border_color(theme.focus).text_color(theme.canvas).child("✓"))
        .when(!checked, |d| d.border_color(theme.line_strong))
}

/// `[−] value [+]`.
fn stepper(
    theme: &Theme,
    id: &'static str,
    value: String,
    down: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
    up: impl Fn(&gpui::ClickEvent, &mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let side = |which: &'static str, text: &'static str| {
        div()
            .id((id, if which == "down" { 0usize } else { 1 }))
            .w(px(32.))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme.muted)
            .cursor_pointer()
            .hover(|s| s.bg(theme.line).text_color(theme.ink))
            .child(text)
    };
    div()
        .flex()
        .items_center()
        .h(px(32.))
        .w(px(150.))
        .rounded_md()
        .border_1()
        .border_color(theme.line_strong)
        .bg(theme.canvas)
        .overflow_hidden()
        .child(side("down", "−").on_click(down))
        .child(div().flex_1().text_center().text_sm().child(value))
        .child(side("up", "+").on_click(up))
}

impl SettingsView {
    fn render_nav(&self, pane: Pane, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let status = match pane {
            Pane::Jira => Some(&self.statuses.jira),
            Pane::Toggl => Some(&self.statuses.toggl),
            Pane::Calendar => Some(&self.statuses.calendar),
            Pane::Code => Some(&self.statuses.git),
            Pane::Workday => None,
            Pane::Updates => None,
        };
        let dot = match status {
            Some(status) => Some(status.color(theme)),
            None if pane == Pane::Updates && self.statuses.update_ready => Some(theme.focus),
            None => None,
        };
        let selected = pane == self.pane;
        div()
            .id(("pane", pane as usize))
            .flex()
            .items_center()
            .gap_2()
            .px_2p5()
            .py_1p5()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            // On every item, selected or not: gpui only tracks hovering while an element has a
            // hover style, so one added back on leaving the selection would start out stale and
            // keep the old item lit until the mouse moved.
            .hover(|s| s.text_color(theme.ink))
            .when(selected, |d| d.bg(theme.line).font_weight(FontWeight::MEDIUM))
            .when(!selected, |d| d.text_color(theme.muted))
            .on_click(cx.listener(move |this, _, window, cx| this.select(pane, window, cx)))
            .child(div().flex_1().child(pane.title()))
            .when_some(dot, |d, color| d.child(div().size(px(7.)).rounded_full().bg(color)))
            .into_any_element()
    }

    fn error_for(&self, pane: Pane) -> Option<SharedString> {
        self.error.as_ref().filter(|(p, _)| *p == pane).map(|(_, e)| e.clone())
    }

    fn input(&mut self, theme: &Theme, name: &'static str, state: &Entity<EditableTextState>, placeholder: &'static str, window: &Window, cx: &App) -> AnyElement {
        self.order.push(state.clone());
        div()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(label(theme, name))
            .child(field(name, state, placeholder, theme, window, cx))
            .into_any_element()
    }

    /// A connection that's set up: how it's doing, and buttons to change or drop it.
    fn connected(&self, theme: &Theme, status: &Status, detail: Option<String>, actions: Vec<AnyElement>) -> AnyElement {
        let status = match status {
            Status::Off => Status::Working("Connecting".into()),
            status => status.clone(),
        };
        div()
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .border_1()
            .border_color(theme.line_strong)
            .bg(theme.raised)
            .children(status_line(theme, &status))
            .when_some(detail, |d, detail| d.child(div().text_sm().text_color(theme.muted).child(detail)))
            .child(div().flex().gap_2().children(actions))
            .into_any_element()
    }

    fn render_jira(&mut self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let mut body: Vec<AnyElement> = Vec::new();
        if self.editing_jira {
            body.push(
                description(
                    theme,
                    if self.jira_token_saved {
                        "Change where backfill connects to Jira."
                    } else {
                        "Start here. backfill reads your worklogs and issues from Jira, and writes the time you've reviewed back to it. It's the only connection backfill needs."
                    },
                )
                .into_any_element(),
            );
            let site = self.jira_site.clone();
            let email = self.jira_email.clone();
            let token = self.jira_token.clone();
            body.push(self.input(theme, "Site", &site, "yourcompany.atlassian.net", window, cx));
            body.push(self.input(theme, "Email", &email, "you@company.com", window, cx));
            let token_hint = if self.jira_token_saved { "Leave empty to keep the saved token" } else { "Paste your API token" };
            body.push(self.input(theme, "API token", &token, token_hint, window, cx));
            body.push(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(button(theme, "connect-jira", "Connect", true).on_click(cx.listener(|this, _, window, cx| this.connect_jira(window, cx))))
                    .when(self.jira_token_saved, |d| {
                        d.child(button(theme, "cancel-jira", "Cancel", false).on_click(cx.listener(|this, _, window, cx| {
                            this.editing_jira = false;
                            this.error = None;
                            this.focus.focus(window, cx);
                            cx.notify();
                        })))
                    })
                    .child(div().flex_1())
                    .child(link(theme, "jira-token-link", "Create an API token", "https://id.atlassian.com/manage-profile/security/api-tokens"))
                    .into_any_element(),
            );
            body.extend(error_line(theme, self.error_for(Pane::Jira)));
        } else {
            let detail = format!("{} on {}", self.settings.jira_email, self.settings.jira_site);
            let actions = vec![
                button(theme, "change-jira", "Change", false)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.editing_jira = true;
                        this.focus_first_field(window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
                button(theme, "disconnect-jira", "Disconnect", false)
                    .on_click(cx.listener(|this, _, window, cx| this.disconnect_jira(window, cx)))
                    .into_any_element(),
            ];
            body.push(self.connected(theme, &self.statuses.jira, Some(detail), actions));
            body.push(self.render_projects(theme, cx));
        }
        div().flex().flex_col().gap_4().children(body).into_any_element()
    }

    fn render_projects(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let chosen = &self.settings.jira_projects;
        let mut projects: Vec<String> = self.statuses.projects.iter().chain(chosen).map(|p| p.to_uppercase()).collect();
        projects.sort();
        projects.dedup();
        let hint = if chosen.is_empty() {
            "Pick the projects you log time on, and suggestions, the issue search and Fill stay in them. With none picked, they use every project."
        } else {
            "Suggestions, the issue search and Fill stay in these projects. An issue key typed in full still finds any issue."
        };
        group(theme, "Projects you log time on")
            .child(description(theme, hint))
            .when(projects.is_empty(), |d| {
                d.child(div().text_sm().text_color(theme.faint).child("Your projects show here once backfill has read your issues."))
            })
            .child(div().flex().flex_wrap().gap_2().children(projects.into_iter().enumerate().map(|(i, project)| {
                let on = chosen.iter().any(|p| p.eq_ignore_ascii_case(&project));
                div()
                    .id(("project", i))
                    .px_2p5()
                    .py_1()
                    .rounded_full()
                    .border_1()
                    .text_sm()
                    .cursor_pointer()
                    // On every chip, for the reason given in render_nav.
                    .hover(|s| s.border_color(theme.focus))
                    .when(on, |d| d.bg(with_alpha(theme.focus, 0.18)).border_color(theme.focus).text_color(theme.focus))
                    .when(!on, |d| d.border_color(theme.line_strong).text_color(theme.muted))
                    .on_click(cx.listener({
                        let project = project.clone();
                        move |this, _, _, cx| this.toggle_project(&project, cx)
                    }))
                    .child(project)
            })))
            .into_any_element()
    }

    fn render_toggl(&mut self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let mut body: Vec<AnyElement> = vec![description(theme, "Optional. Your Toggl timers show next to your meetings, ready to log on an issue.").into_any_element()];
        if self.editing_toggl {
            let token = self.toggl_token.clone();
            body.push(self.input(theme, "API token", &token, "Paste your API token", window, cx));
            body.push(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(button(theme, "connect-toggl", "Connect", true).on_click(cx.listener(|this, _, window, cx| this.connect_toggl(window, cx))))
                    .when(self.toggl_token_saved, |d| {
                        d.child(button(theme, "cancel-toggl", "Cancel", false).on_click(cx.listener(|this, _, window, cx| {
                            this.editing_toggl = false;
                            this.error = None;
                            this.focus.focus(window, cx);
                            cx.notify();
                        })))
                    })
                    .child(div().flex_1())
                    .child(link(theme, "toggl-token-link", "Find your token in your Toggl profile", "https://track.toggl.com/profile"))
                    .into_any_element(),
            );
            body.extend(error_line(theme, self.error_for(Pane::Toggl)));
        } else {
            let actions = vec![
                button(theme, "change-toggl", "Change token", false)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.editing_toggl = true;
                        this.focus_first_field(window, cx);
                        cx.notify();
                    }))
                    .into_any_element(),
                button(theme, "disconnect-toggl", "Disconnect", false)
                    .on_click(cx.listener(|this, _, window, cx| this.disconnect_toggl(window, cx)))
                    .into_any_element(),
            ];
            body.push(self.connected(theme, &self.statuses.toggl, None, actions));
        }
        div().flex().flex_col().gap_4().children(body).into_any_element()
    }

    fn render_calendar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let fix = self.statuses.calendar_fix.map(|fix| match fix {
            CalendarFix::Ask => button(theme, "calendar-access", "Allow access", true)
                .on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::AskForCalendar)))
                .into_any_element(),
            CalendarFix::OpenPrivacy => button(theme, "calendar-access", "Open Privacy & Security", true)
                .on_click(|_, _, cx| cx.open_url("x-apple.systempreferences:com.apple.preference.security?Privacy_Calendars"))
                .into_any_element(),
        });
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(description(
                theme,
                "Your Outlook meetings come from the Calendar app on this Mac, which syncs your Outlook account. Nothing leaves your Mac except the time you log.",
            ))
            .children(status_line(theme, &self.statuses.calendar))
            .children(fix.map(|fix| div().flex().child(fix)))
            .into_any_element()
    }

    fn render_code(&mut self, theme: &Theme, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let folders = self.folders.clone();
        let several = folders.len() > 1;
        let rows = folders.iter().enumerate().map(|(i, folder)| {
            self.order.push(folder.clone());
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(div().flex_1().child(field(("folder", i), folder, "~/git", theme, window, cx)))
                .child(button(theme, ("choose-folder", i), "Choose…", false).on_click(cx.listener(move |this, _, window, cx| this.choose_folder(Some(i), window, cx))))
                .when(several, |d| {
                    d.child(
                        div()
                            .id(("remove-folder", i))
                            .px_2()
                            .text_color(theme.muted)
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.ink))
                            .on_click(cx.listener(move |this, _, _, cx| this.remove_folder(i, cx)))
                            .child("✕"),
                    )
                })
                .into_any_element()
        });
        let folders_group = group(theme, "Folders with your repositories")
            .children(rows.collect::<Vec<_>>())
            .child(
                div()
                    .id("add-folder")
                    .text_sm()
                    .text_color(theme.focus)
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_click(cx.listener(|this, _, window, cx| this.choose_folder(None, window, cx)))
                    .child("Add another folder…"),
            );

        let repos: AnyElement = match &self.repos {
            None => description(theme, "Looking for repositories…").into_any_element(),
            Some(repos) if repos.is_empty() => description(theme, "There are no git repositories in these folders, or the folders inside them.").into_any_element(),
            Some(repos) => {
                let reading = repos.iter().filter(|r| self.settings.tracks(r)).count();
                let all: Vec<_> = repos.iter().map(|r| r.path.clone()).collect();
                let quick = |id: &'static str, text: &'static str, read: bool, cx: &mut Context<Self>| {
                    let all = all.clone();
                    div()
                        .id(id)
                        .text_xs()
                        .text_color(theme.focus)
                        .cursor_pointer()
                        .hover(|s| s.underline())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let paths: Vec<&Path> = all.iter().map(|p| p.as_path()).collect();
                            this.set_reading(&paths, read, cx);
                        }))
                        .child(text)
                };
                let list: Vec<AnyElement> = repos.iter().enumerate().map(|(i, repo)| {
                    let on = self.settings.tracks(repo);
                    let name = repo.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let path = repo.path.clone();
                    div()
                        .id(("repo", i))
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .py_2()
                        .when(i > 0, |d| d.border_t_1().border_color(theme.line))
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.line))
                        .on_click(cx.listener(move |this, _, _, cx| this.set_reading(&[path.as_path()], !on, cx)))
                        .child(tick(theme, on))
                        .child(div().text_sm().when(!on, |d| d.text_color(theme.muted)).child(name))
                        .child(div().flex_1())
                        .children(repo.origin.as_deref().map(|o| div().text_xs().text_color(theme.faint).child(origin_name(o))))
                        .into_any_element()
                }).collect();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(label(theme, "Repositories to read"))
                            .child(div().flex_1())
                            .child(div().text_xs().text_color(theme.muted).child(format!("{reading} of {}", repos.len())))
                            .child(quick("read-all", "All", true, cx))
                            .child(quick("read-none", "None", false, cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .rounded_lg()
                            .border_1()
                            .border_color(theme.line_strong)
                            .bg(theme.raised)
                            .overflow_hidden()
                            .children(list),
                    )
                    .into_any_element()
            }
        };

        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(description(
                theme,
                "backfill looks at your own commits, and the branches you switch to, in the repositories on this Mac, to tell which issue you worked on. No GitHub account needed.",
            ))
            .child(folders_group)
            .child(repos)
            .children(status_line(theme, &self.statuses.git))
            .into_any_element()
    }

    fn render_workday(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let hours = backfill_core::time::format_hours(jiff::SignedDuration::from_mins((self.settings.hours_per_day * 60.).round() as i64));
        let (h, m) = self.settings.start();
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(description(theme, "Each day's heading counts your hours against a full day, and Fill drafts the rest of a day from when it usually starts."))
            .child(group(theme, "A full day").child(stepper(
                theme,
                "hours",
                hours,
                cx.listener(|this, _, _, cx| this.step_hours(-0.25, cx)),
                cx.listener(|this, _, _, cx| this.step_hours(0.25, cx)),
            )))
            .child(group(theme, "Usually starts at").child(stepper(
                theme,
                "start",
                format!("{h:02}:{m:02}"),
                cx.listener(|this, _, _, cx| this.step_start(-15, cx)),
                cx.listener(|this, _, _, cx| this.step_start(15, cx)),
            )))
            .into_any_element()
    }

    fn render_updates(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let version = format!("backfill {}{}", update::VERSION, if update::VERSION.starts_with("0.") { " beta" } else { "" });
        if !self.statuses.updatable {
            return description(theme, format!("This is {version}, built from source, so it doesn't update itself. Installed from GitHub, it does."))
                .into_any_element();
        }
        let on = self.settings.check_for_updates;
        let (focus, line, surface) = (theme.focus, theme.line_strong, theme.surface);
        let busy = matches!(self.statuses.update, Status::Working(_));
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(description(theme, format!("This is {version}. New versions download in the background, and are installed when backfill restarts or quits.")))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        BaseSwitch::new("check-for-updates")
                            .checked(on)
                            .aria_label("Check for updates when backfill opens")
                            .on_change(cx.listener(|this, on: &bool, _, cx| this.set_check_for_updates(*on, cx)))
                            .with_state(move |base, on, _| {
                                base.flex()
                                    .flex_shrink_0()
                                    .w(px(32.))
                                    .h(px(18.))
                                    .p(px(2.))
                                    .rounded_full()
                                    .cursor_pointer()
                                    .bg(if on { focus } else { line })
                                    .when(on, |d| d.justify_end())
                                    .child(BaseSwitchThumb::new("check-for-updates-thumb").size(px(14.)).rounded_full().bg(surface))
                            }),
                    )
                    .child(div().text_sm().child("Check for updates when backfill opens")),
            )
            .children(status_line(theme, &self.statuses.update))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .when(self.statuses.update_ready, |d| {
                        d.child(button(theme, "restart-to-update", "Restart to update", true).on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::RestartToUpdate))))
                    })
                    .when(!self.statuses.update_ready && !busy, |d| {
                        d.child(button(theme, "check-now", "Check now", false).on_click(cx.listener(|_, _, _, cx| cx.emit(SettingsEvent::CheckForUpdates))))
                    }),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gpui::{TestApp, TestAppWindow};
    use gpui_elements::editable_text::actions::{DEFAULT_INPUT_CONTEXT, default_bindings};

    use super::*;

    fn open(pane: Pane) -> (TestApp, TestAppWindow<SettingsView>) {
        let mut app = TestApp::new();
        app.update(|cx| {
            cx.set_reduce_motion(true);
            cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
            bind_keys(cx);
        });
        let settings = Settings { code_folders: Vec::new(), ..Settings::default() };
        let mut window = app.open_window(move |window, cx| SettingsView::new(Store::for_tests(), settings, Statuses::default(), pane, window, cx));
        window.update(|this, window, cx| this.focus_first_field(window, cx));
        window.draw();
        (app, window)
    }

    fn focused(window: &mut TestAppWindow<SettingsView>) -> Option<usize> {
        window.update(|this, window, cx| this.order.iter().position(|f| f.read(cx).focus_handle(cx).is_focused(window)))
    }

    #[test]
    fn tab_moves_through_jira_s_fields_and_enter_connects() {
        let (_app, mut window) = open(Pane::Jira);
        assert_eq!(focused(&mut window), Some(0));
        window.simulate_input("https://example.atlassian.net/jira/your-work");
        window.simulate_keystrokes("tab");
        window.simulate_input("me@example.com");
        window.simulate_keystrokes("tab");
        window.simulate_input("secret");
        window.simulate_keystrokes("enter");
        window.read(|this, _| {
            assert_eq!(this.settings.jira_site, "example.atlassian.net");
            assert_eq!(this.settings.jira_email, "me@example.com");
            assert!(this.jira_token_saved);
            assert!(!this.editing_jira);
            assert_eq!(this.store.secret(Secret::JiraToken).as_deref(), Some("secret"));
        });
    }

    #[test]
    fn connecting_without_a_token_says_what_is_missing() {
        let (_app, mut window) = open(Pane::Jira);
        window.simulate_input("example.atlassian.net");
        window.simulate_keystrokes("enter");
        window.read(|this, _| {
            assert!(this.editing_jira);
            assert!(this.error_for(Pane::Jira).is_some_and(|e| e.contains("API token")));
        });
    }

    #[test]
    fn tab_wraps_around() {
        let (_app, mut window) = open(Pane::Jira);
        window.simulate_keystrokes("shift-tab");
        assert_eq!(focused(&mut window), Some(2));
        window.simulate_keystrokes("tab");
        assert_eq!(focused(&mut window), Some(0));
    }

    #[test]
    fn the_workday_steps_by_quarter_hours() {
        let (_app, mut window) = open(Pane::Workday);
        window.update(|this, _, cx| {
            this.step_hours(0.25, cx);
            this.step_start(-15, cx);
        });
        window.read(|this, _| {
            assert_eq!(this.settings.hours_per_day, 7.75);
            assert_eq!(this.settings.day_start, "07:45");
        });
        window.update(|this, _, cx| (0..200).for_each(|_| this.step_start(-15, cx)));
        window.read(|this, _| assert_eq!(this.settings.day_start, "00:00"));
    }

    #[test]
    fn picking_projects_adds_and_removes_them() {
        let (_app, mut window) = open(Pane::Jira);
        window.update(|this, _, cx| {
            this.toggle_project("APP", cx);
            this.toggle_project("SUP", cx);
            this.toggle_project("app", cx);
        });
        window.read(|this, _| assert_eq!(this.settings.jira_projects, ["SUP"]));
    }

    #[test]
    fn unticking_a_repository_stops_reading_it_and_drops_old_organisations() {
        let (_app, mut window) = open(Pane::Code);
        let repo = |path: &str, owner: &str| Repo { path: PathBuf::from(path), origin: Some(format!("git@github.com:{owner}/x.git")) };
        window.update(|this, _, cx| {
            this.settings.organisations = vec!["acme".into()];
            this.repos = Some(vec![repo("/code/a", "acme"), repo("/code/b", "acme"), repo("/code/c", "other")]);
            this.set_reading(&[Path::new("/code/b")], false, cx);
        });
        window.read(|this, _| {
            // c was left out by the organisation, and b now by choice.
            assert!(this.settings.organisations.is_empty());
            assert_eq!(this.settings.ignored_repositories, ["/code/b", "/code/c"]);
        });
        window.update(|this, _, cx| this.set_reading(&[Path::new("/code/c")], true, cx));
        window.read(|this, _| assert_eq!(this.settings.ignored_repositories, ["/code/b"]));
    }

    #[test]
    fn reads_sites_and_remotes_however_they_are_written() {
        assert_eq!(clean_site(" https://example.atlassian.net/jira/your-work "), "example.atlassian.net");
        assert_eq!(clean_site("example.atlassian.net"), "example.atlassian.net");
        assert_eq!(origin_name("git@github.com:acme/app.git"), "acme/app");
        assert_eq!(origin_name("https://github.com/acme/app"), "acme/app");
    }
}
