//! The `L` popover: type to search issues, ranked by what you were likely working on.

use backfill_core::{Issue, IssueKey, IssueKind, suggest::Reason};
use gpui::{
    Animation, AnimationExt, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    FontWeight, KeyBinding, SharedString, Subscription, Window, actions, div, ease_out_quint,
    millis, prelude::*, px,
};
use gpui_elements::editable_text::{EditableTextState, StringStorage, TextChanged};

use crate::{
    field::field,
    theme::{Theme, with_alpha},
};

actions!(picker, [SelectPrev, SelectNext, Confirm, Cancel]);

const CONTEXT: &str = "IssuePicker";
const VISIBLE_ROWS: usize = 7;
pub const WIDTH: f32 = 400.;

pub fn bind_keys(cx: &mut App) {
    // The text field is the deepest context and binds up/down itself. Registering these
    // after its defaults lets them win the tie.
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, Some("IssuePicker > EditableText")),
        KeyBinding::new("down", SelectNext, Some("IssuePicker > EditableText")),
        KeyBinding::new("ctrl-p", SelectPrev, Some(CONTEXT)),
        KeyBinding::new("ctrl-n", SelectNext, Some(CONTEXT)),
        KeyBinding::new("enter", Confirm, Some(CONTEXT)),
        KeyBinding::new("escape", Cancel, Some(CONTEXT)),
        KeyBinding::new("escape", Cancel, Some("IssuePicker > EditableText")),
    ]);
}

pub struct Row {
    pub issue: Issue,
    pub reasons: Vec<Reason>,
    /// The search text Jira found this issue for. It stays listed for that text even when
    /// it matched in a place the local filter can't see, like the description.
    pub found_for: Option<String>,
    /// Outside the projects time is logged on. Only a typed key brings these up.
    pub outside: bool,
}

/// Where the Jira search for some text stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Search {
    /// Nothing is being looked up, as when Jira isn't connected or the text is too short.
    #[default]
    Idle,
    Running(String),
    Done(String),
    Failed(String),
}

pub enum PickerEvent {
    Picked(IssueKey),
    Dismissed,
    /// The search text changed, so issues beyond the suggestions can be looked up.
    Query(String),
}

pub struct IssuePicker {
    title: SharedString,
    verb: SharedString,
    query: Entity<EditableTextState>,
    rows: Vec<Row>,
    visible: Vec<usize>,
    selected: usize,
    search: Search,
    /// The projects time is logged on, if limited, for saying what was searched.
    scope: Vec<String>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<PickerEvent> for IssuePicker {}

impl Focusable for IssuePicker {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.query.read(cx).focus_handle(cx)
    }
}

impl IssuePicker {
    /// `rows` arrive ranked, best first. `verb` names what Enter does, such as "Link".
    pub fn new(
        title: impl Into<SharedString>,
        verb: impl Into<SharedString>,
        rows: Vec<Row>,
        cx: &mut Context<Self>,
    ) -> Self {
        let query = cx.new(|cx| EditableTextState::new(StringStorage::default(), cx));
        let subscription = cx.subscribe(&query, |this, _, _: &TextChanged, cx| {
            this.refilter(cx);
            let text = this.query.read(cx).as_str().trim().to_owned();
            cx.emit(PickerEvent::Query(text));
            cx.notify();
        });
        let mut picker = Self {
            title: title.into(),
            verb: verb.into(),
            query,
            rows,
            visible: Vec::new(),
            selected: 0,
            search: Search::Idle,
            scope: Vec::new(),
            _subscriptions: vec![subscription],
        };
        picker.refilter(cx);
        picker
    }

    /// What to say under the rows, if anything: how the Jira search is going, or why
    /// nothing is listed. The flag is set when the search failed.
    fn status(&self, cx: &App) -> Option<(String, bool)> {
        let typed = self.query.read(cx).as_str().trim().to_owned();
        let empty = self.visible.is_empty();
        match &self.search {
            Search::Running(q) if *q == typed => Some(("Searching Jira…".into(), false)),
            Search::Failed(q) if *q == typed => Some(("Couldn't search Jira. Check the connection in Settings.".into(), true)),
            Search::Done(q) if *q == typed && empty => {
                let scope = match self.scope.as_slice() {
                    [] => String::new(),
                    projects => format!(" Only {} is searched. Type a full key, like ABC-12, to use another project.", projects.join(" and ")),
                };
                Some((format!("Nothing in Jira matches “{typed}”.{scope}"), false))
            }
            _ if empty => Some(("No issue matches. Try the issue number, like 123.".into(), false)),
            _ => None,
        }
    }

    /// The keys listed, in order.
    #[cfg(test)]
    pub fn listed(&self) -> Vec<String> {
        self.visible.iter().map(|&i| self.rows[i].issue.key.to_string()).collect()
    }

    #[cfg(test)]
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn set_title(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.title = title.into();
        cx.notify();
    }

    pub fn set_scope(&mut self, projects: Vec<String>) {
        self.scope = projects;
    }

    pub fn set_search(&mut self, search: Search, cx: &mut Context<Self>) {
        if self.search != search {
            self.search = search;
            cx.notify();
        }
    }

    /// Takes in what Jira found for `query`: added after the suggestions, keeping the
    /// selection on the same issue.
    pub fn search_finished(&mut self, query: String, found: Option<Vec<Row>>, cx: &mut Context<Self>) {
        let Some(rows) = found else {
            return self.set_search(Search::Failed(query), cx);
        };
        self.search = Search::Done(query);
        let selected = self.visible.get(self.selected).map(|&i| self.rows[i].issue.key.clone());
        for row in rows {
            match self.rows.iter_mut().find(|r| r.issue.key == row.issue.key) {
                Some(existing) => existing.found_for = row.found_for,
                None => self.rows.push(row),
            }
        }
        self.refilter(cx);
        if let Some(key) = selected {
            self.selected = self.visible.iter().position(|&i| self.rows[i].issue.key == key).unwrap_or(0);
        }
        cx.notify();
    }

    /// The search text as typed.
    #[cfg(test)]
    pub fn query_text(&self, cx: &App) -> String {
        self.query.read(cx).as_str().to_owned()
    }

    /// Replaces the search text, as if it had been typed.
    pub fn set_query(&mut self, text: &str, cx: &mut Context<Self>) {
        self.query.update(cx, |query, cx| query.emplace(text, cx));
    }

    fn refilter(&mut self, cx: &App) {
        let query = self.query.read(cx).as_str().trim().to_lowercase();
        self.visible = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, row)| matches(&row.issue, &query) || row.found_for.as_deref().is_some_and(|q| q.to_lowercase() == query))
            .map(|(i, _)| i)
            .take(VISIBLE_ROWS)
            .collect();
        self.selected = 0;
    }

    pub fn select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        cx.notify();
    }

    pub fn select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected + 1 < self.visible.len() {
            self.selected += 1;
        }
        cx.notify();
    }

    pub fn confirm(&mut self, _: &Confirm, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(&index) = self.visible.get(self.selected) {
            cx.emit(PickerEvent::Picked(self.rows[index].issue.key.clone()));
        }
    }

    /// The issue Enter would pick.
    pub fn highlighted(&self) -> Option<IssueKey> {
        self.visible.get(self.selected).map(|&index| self.rows[index].issue.key.clone())
    }

    pub fn cancel(&mut self, _: &Cancel, _: &mut Window, cx: &mut Context<Self>) {
        cx.emit(PickerEvent::Dismissed);
    }
}

/// An empty query matches everything. Digits match the issue number, so `123` finds
/// `APP-123`. Otherwise every word must appear in the key or the summary.
fn matches(issue: &Issue, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let key = issue.key.as_str().to_lowercase();
    if query.chars().all(|c| c.is_ascii_digit()) {
        return key.split_once('-').is_some_and(|(_, number)| number.starts_with(query));
    }
    let summary = issue.summary.to_lowercase();
    query
        .split_whitespace()
        .all(|word| key.contains(word) || summary.contains(word))
}

impl Render for IssuePicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::for_window(window);
        let rows = self.visible.iter().enumerate().map(|(position, &index)| {
            let row = &self.rows[index];
            let selected = position == self.selected;
            let issue = &row.issue;
            let last_resort = issue.kind == IssueKind::WorkLog;
            let detail: SharedString = if last_resort {
                "Work log: use only when no story, task or bug fits".into()
            } else {
                row.outside
                    .then(|| "Outside your projects".to_owned())
                    .into_iter()
                    .chain(row.reasons.iter().map(|r| r.label()))
                    .collect::<Vec<_>>()
                    .join(", ")
                    .into()
            };
            let unit = issue.business_unit.clone().unwrap_or_default();
            div()
                .id(("pick", index))
                .flex()
                .gap_3()
                .px_3()
                .py_2()
                .rounded_md()
                .cursor_pointer()
                .when(selected, |d| d.bg(theme.line))
                .hover(|s| s.bg(with_alpha(theme.line, 0.6)))
                .on_mouse_move(cx.listener(move |this, _, _, cx| {
                    if this.selected != position {
                        this.selected = position;
                        cx.notify();
                    }
                }))
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.emit(PickerEvent::Picked(this.rows[index].issue.key.clone()));
                }))
                .child(
                    div()
                        .w(px(4.))
                        .rounded_full()
                        .flex_shrink_0()
                        .bg(theme.issue(&issue.key)),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .items_baseline()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(theme.ink)
                                        .child(SharedString::from(issue.key.to_string())),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.faint)
                                        .child(issue.kind.label()),
                                )
                                .child(div().flex_1())
                                .child(div().text_xs().text_color(theme.faint).child(unit)),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(if last_resort { theme.muted } else { theme.ink })
                                .truncate()
                                .child(SharedString::from(issue.summary.clone())),
                        )
                        .when(!detail.is_empty(), |d| {
                            d.child(
                                div()
                                    .text_xs()
                                    .text_color(if last_resort || row.outside { theme.amber_ink } else { theme.muted })
                                    .child(detail),
                            )
                        }),
                )
        });

        let empty = self.visible.is_empty();
        let status = self.status(cx).map(|(text, failed)| (SharedString::from(text), if failed { theme.danger } else { theme.muted }));
        div()
            .key_context(CONTEXT)
            .on_action(cx.listener(Self::select_prev))
            .on_action(cx.listener(Self::select_next))
            .on_action(cx.listener(Self::confirm))
            .on_action(cx.listener(Self::cancel))
            .occlude()
            .w(px(WIDTH))
            .flex()
            .flex_col()
            .rounded_xl()
            .bg(theme.raised)
            .border_1()
            .border_color(theme.line_strong)
            .shadow_2xl()
            .overflow_hidden()
            .child(
                div()
                    .px_4()
                    .pt_3()
                    .text_xs()
                    .text_color(theme.muted)
                    .truncate()
                    .child(self.title.clone()),
            )
            .child(div().px_3().pt_2().pb_3().border_b_1().border_color(theme.line).child(field(
                "issue-query",
                &self.query,
                "Search by key or summary, like 123 or login",
                &theme,
                window,
                cx,
            )))
            .child(div().p_1().flex().flex_col().children(rows).when_some(status, |d, (text, color)| {
                d.child(
                    div()
                        .px_3()
                        .when(empty, |d| d.py_4().text_sm())
                        .when(!empty, |d| d.pt_1().pb_2().text_xs())
                        .text_color(color)
                        .child(text),
                )
            }))
            .child(
                div()
                    .flex()
                    .gap_4()
                    .px_4()
                    .py_2()
                    .border_t_1()
                    .border_color(theme.line)
                    .text_xs()
                    .text_color(theme.faint)
                    .child(hint(&theme, "Enter", self.verb.clone()))
                    .child(hint(&theme, "⌘O", "Open in Jira"))
                    .child(hint(&theme, "esc", "Cancel")),
            )
            .with_animation(
                "picker-enter",
                Animation::new(millis(180)).with_easing(ease_out_quint()),
                |el, t| el.opacity(t).mt(px(6. * (1. - t))),
            )
    }
}

pub fn hint(theme: &Theme, key: &'static str, label: impl Into<SharedString>) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .gap_1p5()
        .child(
            div()
                .px_1p5()
                .rounded_sm()
                .border_1()
                .border_color(theme.line_strong)
                .text_color(theme.muted)
                .child(key),
        )
        .child(div().text_color(theme.muted).child(label.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use backfill_core::StatusCategory;

    fn issue(key: &str, summary: &str) -> Issue {
        Issue {
            key: IssueKey::parse(key).unwrap(),
            summary: summary.into(),
            kind: IssueKind::Task,
            status: StatusCategory::InProgress,
            epic: None,
            business_unit: None,
            labels: Vec::new(),
            assigned_to_me: true,
        }
    }

    #[test]
    fn numbers_match_the_issue_number() {
        let i = issue("APP-123", "Fix the export button");
        assert!(matches(&i, "123"));
        assert!(matches(&i, "12"));
        assert!(!matches(&i, "23"));
    }

    #[test]
    fn words_match_key_or_summary_in_any_order() {
        let i = issue("APP-123", "Fix the export button");
        assert!(matches(&i, "export fix"));
        assert!(matches(&i, "app"));
        assert!(!matches(&i, "export invoice"));
    }

    #[test]
    fn says_while_jira_is_searched_and_what_came_of_it() {
        let mut app = gpui::TestApp::new();
        let picker = app.new_entity(|cx| IssuePicker::new("Log", "Log time", Vec::new(), cx));
        let status = |app: &gpui::TestApp| app.read_entity(&picker, |p, cx| p.status(cx).map(|(text, _)| text));
        app.update_entity(&picker, |p, cx| {
            p.set_scope(vec!["APP".into()]);
            p.set_query("summit", cx);
            p.set_search(Search::Running("summit".into()), cx);
        });
        assert_eq!(status(&app).as_deref(), Some("Searching Jira…"));

        let found = Row { issue: issue("EVT-12", "Oslo summit"), reasons: Vec::new(), found_for: Some("summit".into()), outside: true };
        app.update_entity(&picker, |p, cx| p.search_finished("summit".into(), Some(vec![found]), cx));
        assert_eq!(status(&app), None);
        assert_eq!(app.read_entity(&picker, |p, _| p.listed()), ["EVT-12"]);

        app.update_entity(&picker, |p, cx| {
            p.set_query("nothing", cx);
            p.search_finished("nothing".into(), Some(Vec::new()), cx);
        });
        assert_eq!(
            status(&app).as_deref(),
            Some("Nothing in Jira matches “nothing”. Only APP is searched. Type a full key, like ABC-12, to use another project.")
        );
        app.update_entity(&picker, |p, cx| p.search_finished("nothing".into(), None, cx));
        assert_eq!(status(&app).as_deref(), Some("Couldn't search Jira. Check the connection in Settings."));
    }
}
