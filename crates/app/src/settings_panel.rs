//! Settings: where to find Jira, Toggl and your code, and how long your day is.
//!
//! Tokens are never shown once saved: the field says they're in the Keychain and offers to
//! replace them.

use backfill_core::time::format_hours;
use gpui::{
    AnyElement, Animation, AnimationExt, App, Context, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, KeyBinding, ScrollHandle, SharedString, Subscription, Window, actions,
    div, ease_out_quint, millis, prelude::*, px,
};
use gpui_elements::editable_text::{EditableTextState, StringStorage};
use jiff::SignedDuration;

use crate::{
    field::field,
    picker::hint,
    store::{Secret, Settings, Store},
    theme::Theme,
};

actions!(settings, [SaveSettings, CloseSettings, NextField, PrevField]);

const CONTEXT: &str = "Settings";
pub const WIDTH: f32 = 520.;

pub fn bind_keys(cx: &mut App) {
    // The text field binds escape itself; these come after its defaults so they win.
    cx.bind_keys([
        KeyBinding::new("escape", CloseSettings, Some(CONTEXT)),
        KeyBinding::new("escape", CloseSettings, Some("Settings > EditableText")),
        KeyBinding::new("secondary-s", SaveSettings, Some(CONTEXT)),
        KeyBinding::new("enter", SaveSettings, Some("Settings > EditableText")),
        // A single-line field hands Tab on to the panel.
        KeyBinding::new("tab", NextField, Some(CONTEXT)),
        KeyBinding::new("shift-tab", PrevField, Some(CONTEXT)),
    ]);
}

/// How a connection is doing, as shown under its section.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum Status {
    #[default]
    Off,
    Working(SharedString),
    Ok(SharedString),
    Problem(SharedString),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Statuses {
    pub jira: Status,
    pub toggl: Status,
    pub git: Status,
    pub calendar: Status,
}

pub enum SettingsEvent {
    /// Settings were saved; reconnect whatever changed.
    Saved,
    Closed,
}

pub struct SettingsPanel {
    store: Store,
    jira_site: Entity<EditableTextState>,
    jira_email: Entity<EditableTextState>,
    jira_token: Entity<EditableTextState>,
    projects: Entity<EditableTextState>,
    business_unit_field: Entity<EditableTextState>,
    shared_units_field: Entity<EditableTextState>,
    toggl_token: Entity<EditableTextState>,
    folders: Entity<EditableTextState>,
    organisations: Entity<EditableTextState>,
    hours: Entity<EditableTextState>,
    day_start: Entity<EditableTextState>,
    jira_token_saved: bool,
    toggl_token_saved: bool,
    replacing: (bool, bool),
    statuses: Statuses,
    error: Option<SharedString>,
    first_run: bool,
    focus: FocusHandle,
    scroll: ScrollHandle,
    /// The fields on show, in Tab order, with the row each one is in.
    order: Vec<(Entity<EditableTextState>, usize)>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SettingsEvent> for SettingsPanel {}

impl Focusable for SettingsPanel {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        if self.first_run {
            self.jira_site.read(cx).focus_handle(cx)
        } else {
            self.focus.clone()
        }
    }
}

impl SettingsPanel {
    pub fn new(store: Store, settings: &Settings, statuses: Statuses, first_run: bool, cx: &mut Context<Self>) -> Self {
        let field = |text: &str, cx: &mut Context<Self>| cx.new(|cx| EditableTextState::new(StringStorage::from(text), cx));
        let jira_token_saved = store.secret(Secret::JiraToken).is_some();
        let toggl_token_saved = store.secret(Secret::TogglToken).is_some();
        Self {
            jira_site: field(&settings.jira_site, cx),
            jira_email: field(&settings.jira_email, cx),
            jira_token: field("", cx),
            projects: field(&settings.jira_projects.join(", "), cx),
            business_unit_field: field(&settings.business_unit_field, cx),
            shared_units_field: field(&settings.shared_units_field, cx),
            toggl_token: field("", cx),
            folders: field(&settings.code_folders.join(", "), cx),
            organisations: field(&settings.organisations.join(", "), cx),
            hours: field(&format!("{}", settings.hours_per_day), cx),
            day_start: field(&settings.day_start, cx),
            store,
            jira_token_saved,
            toggl_token_saved,
            replacing: (false, false),
            statuses,
            error: None,
            first_run,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            order: Vec::new(),
            _subscriptions: Vec::new(),
        }
    }

    pub fn set_statuses(&mut self, statuses: Statuses, cx: &mut Context<Self>) {
        if self.statuses != statuses {
            self.statuses = statuses;
            cx.notify();
        }
    }

    fn text(&self, field: &Entity<EditableTextState>, cx: &App) -> String {
        field.read(cx).as_str().trim().to_owned()
    }

    fn save(&mut self, _: &SaveSettings, _: &mut Window, cx: &mut Context<Self>) {
        let list = |text: String| text.split([',', '\n']).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect::<Vec<_>>();
        let hours = self.text(&self.hours, cx).replace(',', ".");
        let Ok(hours_per_day) = hours.parse::<f64>().map_err(|_| ()).and_then(|h| if (1. ..=16.).contains(&h) { Ok(h) } else { Err(()) }) else {
            self.error = Some("Hours per day should be a number like 7.5".into());
            cx.notify();
            return;
        };
        let settings = Settings {
            jira_site: self.text(&self.jira_site, cx).trim_start_matches("https://").trim_end_matches('/').to_owned(),
            jira_email: self.text(&self.jira_email, cx),
            jira_projects: list(self.text(&self.projects, cx)),
            business_unit_field: self.text(&self.business_unit_field, cx),
            shared_units_field: self.text(&self.shared_units_field, cx),
            code_folders: list(self.text(&self.folders, cx)),
            organisations: list(self.text(&self.organisations, cx)),
            hours_per_day,
            day_start: self.text(&self.day_start, cx),
        };
        if Settings::parse_start(&settings.day_start).is_none() {
            self.error = Some("The workday start should look like 08:00".into());
            cx.notify();
            return;
        }
        for (secret, field, saved) in [
            (Secret::JiraToken, self.jira_token.clone(), &mut self.jira_token_saved),
            (Secret::TogglToken, self.toggl_token.clone(), &mut self.toggl_token_saved),
        ] {
            let token = field.read(cx).as_str().trim().to_owned();
            if token.is_empty() {
                continue;
            }
            if let Err(error) = self.store.set_secret(secret, &token) {
                self.error = Some(format!("Couldn't save the token in the Keychain: {error}").into());
                cx.notify();
                return;
            }
            *saved = true;
            field.update(cx, |f, cx| f.emplace("", cx));
        }
        self.replacing = (false, false);
        self.error = None;
        self.first_run = false;
        self.store.save("settings.json", &settings);
        cx.emit(SettingsEvent::Saved);
        cx.notify();
    }

    fn close(&mut self, _: &CloseSettings, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(SettingsEvent::Closed);
    }

    fn next_field(&mut self, _: &NextField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(1, window, cx);
    }

    fn prev_field(&mut self, _: &PrevField, window: &mut Window, cx: &mut Context<Self>) {
        self.move_focus(-1, window, cx);
    }

    /// Focuses the field `step` places along, wrapping around, and scrolls it into view.
    fn move_focus(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.order.len() as isize;
        if count == 0 {
            return;
        }
        let current = self.order.iter().position(|(f, _)| f.read(cx).focus_handle(cx).is_focused(window));
        let next = match current {
            Some(i) => (i as isize + step).rem_euclid(count) as usize,
            None if step > 0 => 0,
            None => count as usize - 1,
        };
        let (field, row) = self.order[next].clone();
        field.read(cx).focus_handle(cx).focus(window, cx);
        self.scroll.scroll_to_item(row);
        cx.notify();
    }
}

impl Render for SettingsPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        let label = |text: &'static str| div().text_xs().text_color(theme.muted).child(text);
        let row = |name: &'static str, input: gpui::AnyElement| div().flex().flex_col().gap_1().child(label(name)).child(input);
        let status = |status: &Status| -> gpui::AnyElement {
            let (text, color) = match status {
                Status::Off => return div().into_any_element(),
                Status::Working(text) => (text.clone(), theme.muted),
                Status::Ok(text) => (text.clone(), theme.ok),
                Status::Problem(text) => (text.clone(), theme.danger),
            };
            div().text_xs().text_color(color).child(text).into_any_element()
        };
        let link = |id: &'static str, text: &'static str, url: &'static str| {
            div()
                .id(id)
                .text_xs()
                .text_color(theme.focus)
                .cursor_pointer()
                .hover(|s| s.underline())
                .on_click(move |_, _, cx| cx.open_url(url))
                .child(text)
        };
        let saved_token = |which: u8, cx: &mut Context<Self>| -> AnyElement {
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_2p5()
                .py_1p5()
                .rounded_md()
                .border_1()
                .border_color(theme.line)
                .text_sm()
                .child(div().flex_1().text_color(theme.muted).child("Saved in your Keychain"))
                .child(
                    div()
                        .id(("replace", which as usize))
                        .text_xs()
                        .text_color(theme.focus)
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let field = if which == 0 {
                                this.replacing.0 = true;
                                &this.jira_token
                            } else {
                                this.replacing.1 = true;
                                &this.toggl_token
                            };
                            field.read(cx).focus_handle(cx).focus(window, cx);
                            cx.notify();
                        }))
                        .child("Replace"),
                )
                .into_any_element()
        };
        let section = |title: &'static str| div().pt_2().text_sm().font_weight(FontWeight::SEMIBOLD).child(title).into_any_element();
        let hours_preview = self
            .text(&self.hours, cx)
            .replace(',', ".")
            .parse::<f64>()
            .ok()
            .map(|h| format_hours(SignedDuration::from_mins((h * 60.).round() as i64)));

        let jira_saved = (self.jira_token_saved && !self.replacing.0).then(|| saved_token(0, cx));
        let toggl_saved = (self.toggl_token_saved && !self.replacing.1).then(|| saved_token(1, cx));

        // The body is built row by row so Tab knows which row to scroll to.
        let mut body: Vec<AnyElement> = Vec::new();
        let mut order = Vec::new();
        let mut input = |body: &mut Vec<AnyElement>, name: &'static str, state: &Entity<EditableTextState>, id: &'static str, placeholder: &'static str| {
            order.push((state.clone(), body.len()));
            body.push(row(name, field(id, state, placeholder, &theme, window, cx).into_any_element()).into_any_element());
        };
        body.push(section("Jira"));
        input(&mut body, "Site", &self.jira_site, "jira-site", "yourcompany.atlassian.net");
        input(&mut body, "Email", &self.jira_email, "jira-email", "you@company.com");
        if let Some(saved) = jira_saved {
            body.push(row("API token", saved).into_any_element());
        } else {
            input(&mut body, "API token", &self.jira_token, "jira-token", "Paste the API token");
        }
        body.push(link("jira-token-link", "Create a token at id.atlassian.com", "https://id.atlassian.com/manage-profile/security/api-tokens").into_any_element());
        input(&mut body, "Log time only on these projects", &self.projects, "jira-projects", "All, or for example APP");
        body.push(status(&self.statuses.jira));
        body.push(section("Toggl Track"));
        if let Some(saved) = toggl_saved {
            body.push(row("API token", saved).into_any_element());
        } else {
            input(&mut body, "API token", &self.toggl_token, "toggl-token", "Paste the API token");
        }
        body.push(link("toggl-token-link", "Find your token at the bottom of your Toggl profile", "https://track.toggl.com/profile").into_any_element());
        body.push(status(&self.statuses.toggl));
        body.push(section("Code"));
        input(&mut body, "Folders with your repositories", &self.folders, "folders", "~/git");
        input(
            &mut body,
            "Only repositories from these GitHub organisations",
            &self.organisations,
            "organisations",
            "All, or for example acme",
        );
        body.push(status(&self.statuses.git));
        body.push(section("Workday"));
        order.push((self.hours.clone(), body.len()));
        order.push((self.day_start.clone(), body.len()));
        body.push(
            div()
                .flex()
                .gap_3()
                .child(div().flex_1().child(row("Hours per day", field("hours", &self.hours, "7.5", &theme, window, cx).into_any_element())))
                .child(div().flex_1().child(row("Usually starts at", field("day-start", &self.day_start, "08:00", &theme, window, cx).into_any_element())))
                .into_any_element(),
        );
        if let Some(hours) = hours_preview {
            body.push(div().text_xs().text_color(theme.muted).child(format!("A full day is {hours}")).into_any_element());
        }
        body.push(section("Business units"));
        body.push(
            div()
                .text_xs()
                .text_color(theme.muted)
                .child("If your Jira says which business units an issue serves, name its fields here. Half of an issue's hours are credited to its business unit, the rest split across the shared units.")
                .into_any_element(),
        );
        order.push((self.business_unit_field.clone(), body.len()));
        order.push((self.shared_units_field.clone(), body.len()));
        body.push(
            div()
                .flex()
                .gap_3()
                .child(div().flex_1().child(row(
                    "Business unit field",
                    field("business-unit-field", &self.business_unit_field, "Optional", &theme, window, cx).into_any_element(),
                )))
                .child(div().flex_1().child(row(
                    "Shared units field",
                    field("shared-units-field", &self.shared_units_field, "Optional", &theme, window, cx).into_any_element(),
                )))
                .into_any_element(),
        );
        body.push(section("Outlook"));
        body.push(div().text_xs().text_color(theme.muted).child("Read from the Calendar app on this Mac, which syncs your Outlook account.").into_any_element());
        body.push(status(&self.statuses.calendar));
        self.order = order;

        div()
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::close))
            .on_action(cx.listener(Self::next_field))
            .on_action(cx.listener(Self::prev_field))
            .occlude()
            .w(px(WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .border_l_1()
            .border_color(theme.line_strong)
            .shadow_2xl()
            .child(
                div()
                    .px_5()
                    .pt(px(28.))
                    .pb_3()
                    .border_b_1()
                    .border_color(theme.line)
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Settings"))
                    .when(self.first_run, |d| {
                        d.child(div().pt_1().text_sm().text_color(theme.muted).child(
                            "Connect Jira to see and log your time. Tokens are kept in your Keychain and only sent to Jira and Toggl.",
                        ))
                    }),
            )
            .child(
                div()
                    .id("settings-body")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .overflow_y_scroll()
                    .px_5()
                    .pb_4()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .children(body),
            )
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
                            .id("save-settings")
                            .px_4()
                            .py_2()
                            .rounded_lg()
                            .bg(theme.ink)
                            .text_color(theme.canvas)
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .on_click(cx.listener(|this, _, window, cx| this.save(&SaveSettings, window, cx)))
                            .child("Save and connect"),
                    )
                    .child(
                        div()
                            .id("close-settings")
                            .px_3()
                            .py_2()
                            .rounded_lg()
                            .text_sm()
                            .text_color(theme.muted)
                            .cursor_pointer()
                            .hover(|s| s.text_color(theme.ink))
                            .on_click(cx.listener(|this, _, window, cx| this.close(&CloseSettings, window, cx)))
                            .child("Close"),
                    )
                    .when_some(self.error.clone(), |d, error| d.child(div().flex_1().text_xs().text_color(theme.danger).child(error)))
                    .when(self.error.is_none(), |d| d.child(div().flex_1()))
                    .child(div().text_xs().child(hint(&theme, "⌘S", "Save"))),
            )
            .with_animation("settings-in", Animation::new(millis(260)).with_easing(ease_out_quint()), |d, t| {
                d.opacity(t).mr(px(-36. * (1. - t)))
            })
    }
}

#[cfg(test)]
mod tests {
    use gpui::{TestApp, TestAppWindow};
    use gpui_elements::editable_text::actions::{DEFAULT_INPUT_CONTEXT, default_bindings};

    use super::*;

    fn open() -> (TestApp, TestAppWindow<SettingsPanel>) {
        let mut app = TestApp::new();
        app.update(|cx| {
            cx.set_reduce_motion(true);
            cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
            bind_keys(cx);
        });
        let mut window =
            app.open_window(|_, cx| SettingsPanel::new(Store::for_tests(), &Settings::default(), Statuses::default(), true, cx));
        window.update(|this, window, cx| this.focus_handle(cx).focus(window, cx));
        window.draw();
        (app, window)
    }

    /// Where the focused field is in Tab order.
    fn focused(window: &mut TestAppWindow<SettingsPanel>) -> Option<usize> {
        window.update(|this, window, cx| this.order.iter().position(|(f, _)| f.read(cx).focus_handle(cx).is_focused(window)))
    }

    #[test]
    fn tab_moves_to_the_next_field_instead_of_typing_a_tab() {
        let (_app, mut window) = open();
        assert_eq!(focused(&mut window), Some(0));
        window.simulate_input("example.atlassian.net");
        window.simulate_keystrokes("tab");
        window.simulate_input("me@example.com");
        window.read(|this, cx| {
            assert_eq!(this.jira_site.read(cx).as_str(), "example.atlassian.net");
            assert_eq!(this.jira_email.read(cx).as_str(), "me@example.com");
        });
        window.simulate_keystrokes("shift-tab");
        assert_eq!(focused(&mut window), Some(0));
    }

    #[test]
    fn tab_order_wraps_around() {
        let (_app, mut window) = open();
        let last = window.read(|this, _| this.order.len() - 1);
        window.simulate_keystrokes("shift-tab");
        assert_eq!(focused(&mut window), Some(last));
        window.simulate_keystrokes("tab");
        assert_eq!(focused(&mut window), Some(0));
    }
}
