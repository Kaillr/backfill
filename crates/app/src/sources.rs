//! Reading Jira, Toggl and git. Every function here blocks and takes owned data, so it
//! runs in the background and hands its result back to the UI.

use std::path::PathBuf;

use backfill_core::{
    Activity, ActivityId, Issue, IssueKey, RemoteWorklog, RemoteWorklogId, Source, TimeRange,
    activity::cancelled_title, issue::find_issue_keys, sync::SyncOp,
};
use backfill_http::Http;
use backfill_git::EventKind;
use backfill_jira::{self as jira, Account, Fields, Me};
use backfill_toggl as toggl;
use backfill_core::time::{add_days, at, local_date, week_start};
use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};

/// Who you are in Jira and where its custom fields are. Asked for once per connection.
#[derive(Clone, Debug)]
pub struct JiraIdentity {
    pub me: Me,
    pub fields: Fields,
}

/// `fields` are the names of the business unit and shared units fields, if set.
pub fn jira_identity(account: &Account, fields: &(String, String)) -> Result<JiraIdentity, jira::Error> {
    let http = Http::default();
    let client = jira::Client::new(&http, account);
    Ok(JiraIdentity { me: client.me()?, fields: client.fields(&fields.0, &fields.1)? })
}

/// Everything a Jira pull brings back for a span of weeks.
pub struct JiraPull {
    pub window: TimeRange,
    pub worklogs: Vec<RemoteWorklog>,
    pub issues: Vec<Issue>,
    /// Recently viewed, most recent first.
    pub viewed: Vec<IssueKey>,
    /// Keys mentioned in titles that Jira doesn't know.
    pub unknown: Vec<IssueKey>,
    /// For each week that has started: issues assigned to you then, and issues you moved.
    pub history: Vec<(TimeRange, Vec<IssueKey>, Vec<IssueKey>)>,
}

/// Your worklogs in `window`, the issues you're likely working on, and summaries for any
/// issue mentioned in `mentioned` that isn't known yet.
pub fn pull_jira(
    account: &Account,
    identity: &JiraIdentity,
    window: TimeRange,
    tz: &TimeZone,
    known: &[IssueKey],
    mentioned: &[IssueKey],
) -> Result<JiraPull, jira::Error> {
    let http = Http::default();
    let client = jira::Client::new(&http, account);
    let (me, fields) = (&identity.me, &identity.fields);
    // Suggestions are nice to have: a refused search shouldn't stop the pull, unless the
    // credentials themselves are wrong.
    let optional = |result: Result<Vec<Issue>, jira::Error>| match result {
        Err(jira::Error::BadCredentials) => Err(jira::Error::BadCredentials),
        other => Ok(other.unwrap_or_default()),
    };
    let mut issues = optional(client.search(
        "assignee = currentUser() AND (statusCategory != Done OR resolved >= -30d) ORDER BY updated DESC",
        fields,
        me,
        100,
    ))?;
    let viewed = optional(client.search("issuekey in issueHistory() ORDER BY lastViewed DESC", fields, me, 30))?;
    let viewed_keys = viewed.iter().map(|i| i.key.clone()).collect();
    issues.extend(viewed);
    let worklogs = client.worklogs(me, window, tz)?;

    // What Jira remembers about each week. A week it can't answer is left out, so its
    // suggestions fall back on today's status.
    let mut history = Vec::new();
    let mut week = week_start(local_date(window.start, tz));
    while at(week, 0, 0, tz) < window.end.min(Timestamp::now()) {
        let range = TimeRange::new(at(week, 0, 0, tz), at(add_days(week, 7), 0, 0, tz));
        let [assigned, moved] = history_queries(week).map(|jql| client.search(&jql, fields, me, 100));
        match (assigned, moved) {
            (Ok(assigned), Ok(moved)) => {
                let keys = |issues: &[Issue]| issues.iter().map(|i| i.key.clone()).collect::<Vec<_>>();
                history.push((range, keys(&assigned), keys(&moved)));
                issues.extend(assigned);
                issues.extend(moved);
            }
            (Err(jira::Error::BadCredentials), _) | (_, Err(jira::Error::BadCredentials)) => return Err(jira::Error::BadCredentials),
            _ => {}
        }
        week = add_days(week, 7);
    }

    let have = |key: &IssueKey| known.contains(key) || issues.iter().any(|i| &i.key == key);
    let mut missing: Vec<IssueKey> = Vec::new();
    for key in worklogs.iter().map(|w| &w.issue).chain(mentioned) {
        if !have(key) && !missing.contains(key) {
            missing.push(key.clone());
        }
    }
    for chunk in missing.chunks(50) {
        issues.extend(client.issues(chunk, fields, me)?);
    }
    Ok(JiraPull { window, worklogs, issues, viewed: viewed_keys, unknown: Vec::new(), history })
}

/// The searches for one week, Monday to Sunday: issues assigned to you at some point,
/// and issues you moved to another status.
fn history_queries(week: Date) -> [String; 2] {
    let (from, to) = (week.strftime("%Y/%m/%d"), add_days(week, 6).strftime("%Y/%m/%d"));
    [
        format!(r#"assignee WAS currentUser() DURING ("{from}", "{to}") ORDER BY updated DESC"#),
        format!(r#"status CHANGED BY currentUser() DURING ("{from}", "{to}") ORDER BY updated DESC"#),
    ]
}

/// Runs the searches from [`search_queries`]. Any issue in Jira can turn up, whether
/// you've opened it before or not.
pub fn search_jira(account: &Account, identity: &JiraIdentity, queries: &[(String, usize)]) -> Result<Vec<Issue>, jira::Error> {
    let http = Http::default();
    let client = jira::Client::new(&http, account);
    let mut found = Vec::new();
    for (jql, limit) in queries {
        found.extend(client.search(jql, &identity.fields, &identity.me, *limit)?);
    }
    Ok(found)
}

/// The Jira searches for what's typed, each with how many results to take.
///
/// A full key finds that issue, even outside `allowed`: someone told you to log there.
/// A bare number finds that number in each allowed project, or each project you've
/// worked in. Words search issue text, within `allowed` when set. Characters Jira's text
/// search treats as operators are left out, so "follow-up (Q4)" searches its words.
pub fn search_queries(query: &str, projects: &[String], allowed: &[String]) -> Vec<(String, usize)> {
    let query = query.trim();
    if let Some(key) = IssueKey::parse(&query.to_uppercase()) {
        return vec![(format!("key = {key}"), 1)];
    }
    if !query.is_empty() && query.chars().all(|c| c.is_ascii_digit()) {
        let projects = if allowed.is_empty() { projects } else { allowed };
        return projects.iter().filter_map(|p| IssueKey::parse(&format!("{p}-{query}"))).map(|k| (format!("key = {k}"), 1)).collect();
    }
    let words: String = query.chars().map(|c| if "+-&|!(){}[]^\"~*?\\/:".contains(c) { ' ' } else { c }).collect();
    let words = words.split_whitespace().collect::<Vec<_>>().join(" ");
    if words.chars().count() < 2 {
        return Vec::new();
    }
    let scope = if allowed.is_empty() { String::new() } else { format!("project in ({}) AND ", allowed.join(", ")) };
    vec![(format!("{scope}text ~ \"{words}*\" ORDER BY updated DESC"), 20)]
}

pub fn apply_to_jira(account: &Account, op: &SyncOp, tz: &TimeZone) -> Result<Option<RemoteWorklogId>, jira::Error> {
    let http = Http::default();
    jira::Client::new(&http, account).apply(op, tz)
}

/// One Toggl sync against the cache. Returns the updated cache and what happened.
pub fn sync_toggl(token: &str, mut cache: toggl::Cache, tz: &TimeZone, asked: bool) -> (toggl::Cache, toggl::Sync) {
    let http = Http::default();
    let outcome = toggl::Client::new(&http, token).sync(&mut cache, Timestamp::now(), tz, asked);
    (cache, outcome)
}

/// Cached Toggl entries as calendar items. A running timer runs until now. Entries named
/// after a cancelled meeting are left out, like the meeting itself.
pub fn toggl_activities(cache: &toggl::Cache, now: Timestamp) -> Vec<Activity> {
    cache
        .entries
        .values()
        .filter(|entry| !cancelled_title(&entry.description))
        .filter_map(|entry| {
            let stop = entry.stop.unwrap_or(now);
            (stop > entry.start).then(|| Activity {
                id: ActivityId(stable_id(&format!("toggl:{}", entry.id))),
                source: Source::Toggl,
                title: if entry.description.trim().is_empty() { "Toggl timer".into() } else { entry.description.clone() },
                range: TimeRange::new(entry.start, stop),
                series: None,
                absorbed: false,
            })
        })
        .collect()
}

/// The span of time Toggl activities are replaced over after a sync: all of what Toggl
/// can return.
pub fn toggl_window(now: Timestamp) -> TimeRange {
    TimeRange::new(now - SignedDuration::from_hours(24 * 100), now + SignedDuration::from_hours(24 * 2))
}

/// Your commits in `window` that name an issue, as moments, and the branches you had
/// checked out, each from switching to it until switching away. Both only rank
/// suggestions. Also returns how many repositories were read.
pub fn git_activities(folders: &[PathBuf], organisations: &[String], window: TimeRange, tz: &TimeZone) -> (Vec<Activity>, usize) {
    let repos = backfill_git::discover(folders, organisations);
    let emails = backfill_git::emails(&repos);
    let mut activities = Vec::new();
    for repo in &repos {
        let events = backfill_git::events(repo, &emails, window.start, window.end);
        let switches: Vec<&backfill_git::Event> = events.iter().filter(|e| e.kind == EventKind::Checkout).collect();
        for event in &events {
            if find_issue_keys(&event.text).is_empty() {
                continue;
            }
            let (source, range) = match event.kind {
                EventKind::Commit => (Source::Git, TimeRange::starting_at(event.at, SignedDuration::from_mins(1))),
                EventKind::Checkout => {
                    let next = switches.iter().map(|e| e.at).find(|at| *at > event.at);
                    (Source::Branch, branch_range(event.at, next, tz))
                }
            };
            let id = stable_id(&format!("git:{}:{}:{}", repo.path.display(), event.at.as_second(), event.text));
            activities.push(Activity { id: ActivityId(id), source, title: event.text.clone(), range, series: None, absorbed: false });
        }
    }
    (activities, repos.len())
}

/// How long a branch counts as the one you were on: until the next switch in that
/// repository, but no later than the end of that day and no longer than ten hours.
fn branch_range(from: Timestamp, next_switch: Option<Timestamp>, tz: &TimeZone) -> TimeRange {
    let end_of_day = backfill_core::time::day_range(backfill_core::time::local_date(from, tz), tz).end;
    let end = [Some(end_of_day), Some(from + SignedDuration::from_hours(10)), next_switch].into_iter().flatten().min().unwrap_or(end_of_day);
    TimeRange::new(from, end)
}

/// An id that stays the same across reloads. The top bit keeps it clear of the ids the
/// workspace hands out itself.
pub fn stable_id(text: &str) -> u64 {
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    (1 << 63) | (hash >> 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn asks_jira_about_a_week_from_monday_to_sunday() {
        let [assigned, moved] = history_queries(jiff::civil::date(2026, 10, 19));
        assert_eq!(assigned, r#"assignee WAS currentUser() DURING ("2026/10/19", "2026/10/25") ORDER BY updated DESC"#);
        assert_eq!(moved, r#"status CHANGED BY currentUser() DURING ("2026/10/19", "2026/10/25") ORDER BY updated DESC"#);
    }

    #[test]
    fn a_branch_counts_until_the_next_switch_or_the_end_of_that_day() {
        let tz = TimeZone::UTC;
        let switched = at("2026-10-26T09:00:00Z");
        assert_eq!(branch_range(switched, Some(at("2026-10-26T11:30:00Z")), &tz).end, at("2026-10-26T11:30:00Z"));
        assert_eq!(branch_range(switched, None, &tz).end, at("2026-10-26T19:00:00Z"));
        assert_eq!(branch_range(at("2026-10-26T20:00:00Z"), None, &tz).end, at("2026-10-27T00:00:00Z"));
    }

    #[test]
    fn searches_stay_inside_the_allowed_projects_unless_a_full_key_is_typed() {
        let worked_in = ["APP".to_owned(), "SUP".to_owned()];
        let only_app = ["APP".to_owned()];
        let jql = |query: &str, allowed: &[String]| -> Vec<String> {
            search_queries(query, &worked_in, allowed).into_iter().map(|(jql, _)| jql).collect()
        };
        assert_eq!(jql("123", &[]), ["key = APP-123", "key = SUP-123"]);
        assert_eq!(jql("123", &only_app), ["key = APP-123"]);
        assert_eq!(jql("sup-55", &only_app), ["key = SUP-55"]);
        assert_eq!(jql("app-123", &only_app), ["key = APP-123"]);
        assert_eq!(jql("export", &only_app), [r#"project in (APP) AND text ~ "export*" ORDER BY updated DESC"#]);
        assert_eq!(jql("export", &[]), [r#"text ~ "export*" ORDER BY updated DESC"#]);
        assert_eq!(jql("follow-up (Q4)", &[]), [r#"text ~ "follow up Q4*" ORDER BY updated DESC"#]);
        assert_eq!(jql("a", &[]), Vec::<String>::new());
    }

    #[test]
    fn toggl_entries_become_calendar_items_and_a_running_timer_runs_until_now() {
        let mut cache = toggl::Cache::default();
        for (id, description, start, stop) in [
            (1, "APP-123 review", "2026-10-26T08:00:00Z", Some("2026-10-26T09:00:00Z")),
            (2, "", "2026-10-26T10:00:00Z", None),
            (3, "Avlyst: Team standup", "2026-10-26T11:00:00Z", Some("2026-10-26T11:30:00Z")),
        ] {
            cache.entries.insert(
                id,
                toggl::Entry {
                    id,
                    description: description.into(),
                    start: at(start),
                    stop: stop.map(at),
                    project_id: None,
                    tags: Vec::new(),
                },
            );
        }
        let items = toggl_activities(&cache, at("2026-10-26T10:30:00Z"));
        assert_eq!(items[0].title, "APP-123 review");
        assert_eq!(items[0].issue_hint().map(|k| k.to_string()), Some("APP-123".into()));
        assert_eq!(items[1].title, "Toggl timer");
        assert_eq!(items[1].range.end, at("2026-10-26T10:30:00Z"));
        assert_eq!(items.len(), 2, "the cancelled meeting's entry is left out");
        assert_eq!(items[0].id, toggl_activities(&cache, at("2026-10-27T00:00:00Z"))[0].id);
    }
}
