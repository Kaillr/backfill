//! Which issues you were probably working on, for a given stretch of time.
//!
//! Everything is weighed against the date being logged, not today: the branch you were
//! on and the commits you made then, what Jira says you were assigned and moved that
//! week, what you logged on nearby days, and issues named in Toggl or meetings that day.

use jiff::{Timestamp, civil::Date, tz::TimeZone};

use crate::{
    Activity, Issue, IssueKey, IssueKind, JiraHistory, Source, StatusCategory, TimeRange, Worklog,
    issue::find_issue_keys, time::local_date,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Reason {
    /// The activity being logged names this issue.
    Mentioned,
    /// A Toggl entry or meeting the same day names it.
    NamedThatDay,
    /// Its branch was checked out at that time.
    OnBranch,
    /// You committed to it during that time.
    CommittedThen,
    /// You made this many commits to it that day.
    CommitsThatDay(usize),
    /// You committed to it within a few days.
    CommitsNearby,
    /// Jira says it was assigned to you that week.
    AssignedThen,
    /// Jira says you moved it to another status that week.
    MovedThen,
    InProgress,
    Assigned,
    /// You logged time on it the same day.
    LoggedThatDay,
    /// You logged time on it within a week, before or after.
    LoggedNearby,
    Viewed,
}

impl Reason {
    pub fn label(self) -> String {
        match self {
            Reason::Mentioned => "in title".into(),
            Reason::NamedThatDay => "named that day".into(),
            Reason::OnBranch => "on its branch then".into(),
            Reason::CommittedThen => "committed then".into(),
            Reason::CommitsThatDay(1) => "1 commit that day".into(),
            Reason::CommitsThatDay(n) => format!("{n} commits that day"),
            Reason::CommitsNearby => "commits nearby".into(),
            Reason::AssignedThen => "assigned to you then".into(),
            Reason::MovedThen => "you moved it that week".into(),
            Reason::InProgress => "in progress".into(),
            Reason::Assigned => "assigned".into(),
            Reason::LoggedThatDay => "logged that day".into(),
            Reason::LoggedNearby => "logged nearby".into(),
            Reason::Viewed => "viewed".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub key: IssueKey,
    pub score: i32,
    pub reasons: Vec<Reason>,
}

pub struct SuggestContext<'a> {
    pub issues: &'a [Issue],
    pub activities: &'a [Activity],
    pub worklogs: &'a [Worklog],
    /// Issues you recently opened in Jira.
    pub viewed: &'a [IssueKey],
    pub history: &'a JiraHistory,
    /// The time being linked: a meeting, a gap, a whole day.
    pub focus: TimeRange,
    /// An issue the focused activity names in its title.
    pub hint: Option<&'a IssueKey>,
    pub tz: &'a TimeZone,
}

/// Every issue, best match first. Issues with no signal at all keep their original order
/// at the end, so typing in the picker can still find them.
pub fn rank(cx: &SuggestContext) -> Vec<Suggestion> {
    let mut ranked: Vec<Suggestion> = cx.issues.iter().map(|issue| score(issue, cx)).collect();
    ranked.sort_by_key(|s| std::cmp::Reverse(s.score));
    ranked
}

fn score(issue: &Issue, cx: &SuggestContext) -> Suggestion {
    let mut score = 0;
    let mut reasons = Vec::new();
    let mut add = |points: i32, reason: Option<Reason>| {
        score += points;
        if let Some(reason) = reason {
            reasons.push(reason);
        }
    };
    let day = local_date(cx.focus.start, cx.tz);
    let days_from = |at: Timestamp| days_apart(local_date(at, cx.tz), day);
    let names = |a: &Activity| find_issue_keys(&a.title).contains(&issue.key);
    let mentioned = cx.hint == Some(&issue.key);

    if mentioned {
        add(200, Some(Reason::Mentioned));
    }

    let (git_points, git_reasons) = git(issue, cx, &days_from);
    for (i, reason) in git_reasons.into_iter().enumerate() {
        add(if i == 0 { git_points } else { 10 }, Some(reason));
    }

    let named_that_day = cx
        .activities
        .iter()
        .any(|a| matches!(a.source, Source::Outlook | Source::Toggl) && days_from(a.range.start) == 0 && names(a));
    if named_that_day && !mentioned {
        add(50, Some(Reason::NamedThatDay));
    }

    // What Jira says about that week, when it's been asked. Otherwise, what's true now.
    if cx.history.covers(&cx.focus) {
        if cx.history.moved(&issue.key, &cx.focus) {
            add(40, Some(Reason::MovedThen));
        }
        if cx.history.assigned(&issue.key, &cx.focus) {
            add(30, Some(Reason::AssignedThen));
        }
        if issue.assigned_to_me && issue.status == StatusCategory::InProgress {
            add(10, Some(Reason::InProgress));
        }
    } else if issue.assigned_to_me {
        match issue.status {
            StatusCategory::InProgress => add(35, Some(Reason::InProgress)),
            StatusCategory::ToDo => add(10, Some(Reason::Assigned)),
            StatusCategory::Done => add(5, Some(Reason::Assigned)),
        }
    } else if issue.status == StatusCategory::InProgress {
        add(5, None);
    }

    let nearest_log = cx.worklogs.iter().filter(|w| w.issue == issue.key).map(|w| days_from(w.range.start)).min();
    match nearest_log {
        Some(0) => add(40, Some(Reason::LoggedThatDay)),
        Some(1..=3) => add(30, Some(Reason::LoggedNearby)),
        Some(4..=7) => add(20, Some(Reason::LoggedNearby)),
        _ => {}
    }

    if cx.viewed.contains(&issue.key) {
        add(15, Some(Reason::Viewed));
    }

    match issue.kind {
        // Work log issues are for time nothing else fits, so they come last.
        IssueKind::WorkLog => add(-40, None),
        // Time goes on the stories under an epic, not on the epic itself.
        IssueKind::Epic => add(-60, None),
        _ => {}
    }

    Suggestion {
        key: issue.key.clone(),
        score,
        reasons,
    }
}

/// Git's points for `issue`, from the strongest of: the branch you were on then, commits
/// during that time, commits that day, commits within a few days. Each kind that applies
/// is a reason; the strongest comes first and carries the points.
fn git(issue: &Issue, cx: &SuggestContext, days_from: &dyn Fn(Timestamp) -> i64) -> (i32, Vec<Reason>) {
    let ours = |a: &&Activity| find_issue_keys(&a.title).contains(&issue.key);
    let mut found: Vec<(i32, Reason)> = Vec::new();
    if cx.activities.iter().filter(ours).any(|a| a.source == Source::Branch && a.range.overlaps(&cx.focus)) {
        found.push((90, Reason::OnBranch));
    }
    let commits: Vec<&Activity> = cx.activities.iter().filter(|a| a.source == Source::Git).filter(ours).collect();
    let that_day = commits.iter().filter(|c| days_from(c.range.start) == 0).count();
    if commits.iter().any(|c| c.range.overlaps(&cx.focus)) {
        found.push((100, Reason::CommittedThen));
    } else if that_day > 0 {
        found.push(((55 + 5 * that_day as i32).min(80), Reason::CommitsThatDay(that_day)));
    } else if commits.iter().any(|c| days_from(c.range.start) <= 3) {
        found.push((30, Reason::CommitsNearby));
    }
    found.sort_by_key(|(points, _)| std::cmp::Reverse(*points));
    let points = found.first().map_or(0, |(p, _)| *p);
    (points, found.into_iter().map(|(_, r)| r).collect())
}

fn days_apart(a: Date, b: Date) -> i64 {
    (a - b).get_days().abs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActivityId, WorklogId, time::at};
    use jiff::{civil::date, tz::TimeZone};

    fn key(s: &str) -> IssueKey {
        IssueKey::parse(s).unwrap()
    }

    fn issue(k: &str, kind: IssueKind, status: StatusCategory, assigned: bool) -> Issue {
        Issue {
            key: key(k),
            summary: k.to_owned(),
            kind,
            status,
            epic: None,
            business_unit: None,
            labels: Vec::new(),
            assigned_to_me: assigned,
        }
    }

    fn range(day: i8, from: i8, to: i8) -> TimeRange {
        let d = date(2026, 10, day);
        TimeRange::new(at(d, from, 0, &TimeZone::UTC), at(d, to, 0, &TimeZone::UTC))
    }

    fn activity(id: u64, source: Source, title: &str, range: TimeRange) -> Activity {
        Activity { id: ActivityId(id), source, title: title.into(), range, series: None, absorbed: false }
    }

    fn worklog(id: u64, k: &str, range: TimeRange) -> Worklog {
        Worklog { id: WorklogId(id), remote_id: None, issue: key(k), range, comment: String::new(), origin: None }
    }

    /// Ranks `issues` for 10:00–11:00 on Tuesday 27 October.
    fn ranked(issues: &[Issue], activities: &[Activity], worklogs: &[Worklog], history: &JiraHistory) -> Vec<Suggestion> {
        rank(&SuggestContext {
            issues,
            activities,
            worklogs,
            viewed: &[],
            history,
            focus: range(27, 10, 11),
            hint: None,
            tz: &TimeZone::UTC,
        })
    }

    fn keys(ranked: &[Suggestion]) -> Vec<String> {
        ranked.iter().map(|s| s.key.to_string()).collect()
    }

    fn todo(k: &str) -> Issue {
        issue(k, IssueKind::Task, StatusCategory::ToDo, false)
    }

    #[test]
    fn a_commit_at_that_time_beats_everything_but_the_title() {
        let issues = [
            issue("AB-1", IssueKind::Story, StatusCategory::InProgress, true),
            todo("AB-2"),
            issue("AB-3", IssueKind::Task, StatusCategory::ToDo, true),
        ];
        let activities = [activity(1, Source::Git, "AB-2: Fix the export", range(27, 10, 11))];
        let history = JiraHistory::default();
        assert_eq!(keys(&ranked(&issues, &activities, &[], &history)), ["AB-2", "AB-1", "AB-3"]);

        let hint = key("AB-3");
        let with_hint = rank(&SuggestContext {
            issues: &issues,
            activities: &activities,
            worklogs: &[],
            viewed: &[],
            history: &history,
            focus: range(27, 10, 11),
            hint: Some(&hint),
            tz: &TimeZone::UTC,
        });
        assert_eq!(keys(&with_hint)[0], "AB-3");
    }

    #[test]
    fn the_branch_you_were_on_then_ranks_high() {
        let issues = [issue("AB-1", IssueKind::Story, StatusCategory::InProgress, true), todo("AB-2")];
        let activities = [activity(1, Source::Branch, "AB-2-export", range(27, 9, 12))];
        let ranked = ranked(&issues, &activities, &[], &JiraHistory::default());
        assert_eq!(keys(&ranked), ["AB-2", "AB-1"]);
        assert_eq!(ranked[0].reasons, [Reason::OnBranch]);
    }

    #[test]
    fn commits_count_by_how_many_and_how_close() {
        let issues = [todo("AB-1"), todo("AB-2"), todo("AB-3")];
        let activities = [
            activity(1, Source::Git, "AB-2: one", range(27, 14, 14)),
            activity(2, Source::Git, "AB-2: two", range(27, 15, 15)),
            activity(3, Source::Git, "AB-2: three", range(27, 16, 16)),
            activity(4, Source::Git, "AB-3: earlier", range(25, 12, 12)),
            activity(5, Source::Git, "AB-1: long ago", range(12, 12, 12)),
        ];
        let ranked = ranked(&issues, &activities, &[], &JiraHistory::default());
        assert_eq!(keys(&ranked), ["AB-2", "AB-3", "AB-1"]);
        assert_eq!(ranked[0].reasons, [Reason::CommitsThatDay(3)]);
        assert_eq!(Reason::CommitsThatDay(3).label(), "3 commits that day");
        assert_eq!(ranked[1].reasons, [Reason::CommitsNearby]);
        assert!(ranked[2].reasons.is_empty());
    }

    #[test]
    fn what_jira_says_about_that_week_beats_todays_status() {
        // AB-1 is in progress today. AB-2 is done now, but that week it was yours and you
        // moved it along.
        let issues = [
            issue("AB-1", IssueKind::Story, StatusCategory::InProgress, true),
            issue("AB-2", IssueKind::Story, StatusCategory::Done, true),
        ];
        let week = TimeRange::new(range(26, 0, 1).start, range(26, 0, 1).start + jiff::SignedDuration::from_hours(24 * 7));
        let mut history = JiraHistory::default();
        history.record(week, vec![key("AB-2")], vec![key("AB-2")]);
        let ranked = ranked(&issues, &[], &[], &history);
        assert_eq!(keys(&ranked), ["AB-2", "AB-1"]);
        assert_eq!(ranked[0].reasons, [Reason::MovedThen, Reason::AssignedThen]);

        // Without Jira's answer for that week, today's status is all there is.
        assert_eq!(keys(&self::ranked(&issues, &[], &[], &JiraHistory::default())), ["AB-1", "AB-2"]);
    }

    #[test]
    fn time_logged_on_nearby_days_counts_before_or_after() {
        let issues = [todo("AB-1"), todo("AB-2"), todo("AB-3")];
        let worklogs = [worklog(1, "AB-3", range(29, 9, 10)), worklog(2, "AB-2", range(27, 13, 15)), worklog(3, "AB-1", range(10, 9, 10))];
        let ranked = ranked(&issues, &[], &worklogs, &JiraHistory::default());
        assert_eq!(keys(&ranked), ["AB-2", "AB-3", "AB-1"]);
        assert_eq!(ranked[0].reasons, [Reason::LoggedThatDay]);
        assert_eq!(ranked[1].reasons, [Reason::LoggedNearby]);
    }

    #[test]
    fn an_issue_named_in_toggl_or_a_meeting_that_day_counts() {
        let issues = [todo("AB-1"), todo("AB-2")];
        let activities = [activity(1, Source::Toggl, "AB-2 review", range(27, 14, 15))];
        let ranked = ranked(&issues, &activities, &[], &JiraHistory::default());
        assert_eq!(keys(&ranked), ["AB-2", "AB-1"]);
        assert_eq!(ranked[0].reasons, [Reason::NamedThatDay]);
    }

    #[test]
    fn work_log_issues_and_epics_sink_to_the_bottom() {
        let issues = [
            issue("AB-5", IssueKind::WorkLog, StatusCategory::InProgress, true),
            issue("AB-100", IssueKind::Epic, StatusCategory::InProgress, true),
            issue("AB-7", IssueKind::Task, StatusCategory::ToDo, true),
        ];
        assert_eq!(keys(&ranked(&issues, &[], &[], &JiraHistory::default())), ["AB-7", "AB-5", "AB-100"]);
    }
}
