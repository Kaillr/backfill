//! A believable two weeks of made-up work, for trying the app without any accounts.
//!
//! Everything lives in a fake `DEMO` project so nothing here can be mistaken for real
//! Jira data.

use jiff::{SignedDuration, civil::Date, tz::TimeZone};

use crate::{
    Activity, ActivityId, Issue, IssueKey, IssueKind, RemoteWorklogId, Source,
    StatusCategory, TimeRange,
    time::{self, add_days, week_start},
    workspace::{Settings, Workspace},
};

/// A workspace covering this week and last week, arranged around `today` so there is
/// always something to do: the two workdays before today and today itself are left to
/// backfill, everything earlier is logged and synced, and last Monday has two changes
/// waiting to sync.
pub fn workspace(today: Date, tz: TimeZone) -> Workspace {
    let mut ws = Workspace::new(tz, Settings::default());
    ws.issues = issues();
    ws.viewed = vec![key("DEMO-130"), key("DEMO-110")];

    let this_week = week_start(today);
    let last_week = add_days(this_week, -7);
    let mut past_workdays = Vec::new();
    for monday in [last_week, this_week] {
        for (offset, plan) in week_plan().into_iter().enumerate() {
            let date = add_days(monday, offset as i64);
            add_day(&mut ws, date, &plan, date <= today);
            if date < today {
                past_workdays.push(date);
            }
        }
    }

    // Log everything but the two most recent workdays. Last Monday is handled below.
    let logged = past_workdays.len().saturating_sub(2);
    for &date in &past_workdays[1..logged] {
        log_day(&mut ws, date);
    }

    // Last Monday was split across two issues and synced. Afterwards the boundary between
    // them moved, so two updates are waiting.
    let tz = ws.tz.clone();
    let at = |h, m| time::at(last_week, h, m, &tz);
    let morning = ws.add_worklog(key("DEMO-97"), TimeRange::new(at(8, 0), at(11, 30)), None);
    let afternoon = ws.add_worklog(key("DEMO-101"), TimeRange::new(at(11, 30), at(15, 30)), None);
    for op in ws.sync_plan() {
        let id = RemoteWorklogId(format!("demo-{}", ws.next_id()));
        ws.record_synced(&op, Some(id));
    }
    let moved = SignedDuration::from_mins(15);
    ws.set_range(morning, TimeRange::new(at(8, 0), at(11, 30) + moved));
    ws.set_range(afternoon, TimeRange::new(at(11, 30) + moved, at(15, 30)));

    ws
}

/// Logs a day the way Fill does: meetings that name an issue on that issue,
/// the rest on the primary task. The result counts as synced.
fn log_day(ws: &mut Workspace, date: Date) {
    if let Some(primary) = ws.primary_issue(date) {
        ws.fill_day(date, primary);
    }
    for op in ws.sync_plan() {
        let id = RemoteWorklogId(format!("demo-{}", ws.next_id()));
        ws.record_synced(&op, Some(id));
    }
}

fn key(s: &str) -> IssueKey {
    IssueKey::parse(s).expect("demo keys are valid")
}

fn issues() -> Vec<Issue> {
    use IssueKind::*;
    use StatusCategory::*;
    let issue = |k: &str, summary: &str, kind, status, epic: Option<&str>, bu: &str, labels: &[&str], mine| Issue {
        key: key(k),
        summary: summary.to_owned(),
        kind,
        status,
        epic: epic.map(key),
        business_unit: Some(bu.to_owned()),
        labels: labels.iter().map(|l| (*l).to_owned()).collect(),
        assigned_to_me: mine,
    };
    vec![
        issue("DEMO-101", "Lay out overlapping calendar blocks", Story, InProgress, Some("DEMO-100"), "Central", &["Central"], true),
        issue("DEMO-104", "Review sheet before syncing worklogs", Task, InProgress, Some("DEMO-100"), "Central", &["Central"], true),
        issue("DEMO-110", "Week total is off by one day", Bug, ToDo, None, "North", &["East", "South", "North"], true),
        issue("DEMO-97", "Export invoices to the warehouse", Story, Done, None, "South", &["South"], true),
        issue("DEMO-120", "Plan Q4 integrations", Task, ToDo, Some("DEMO-100"), "Central", &["Central"], false),
        issue("DEMO-130", "Search customers by name", Story, InProgress, None, "West", &["West", "South"], false),
        issue("DEMO-100", "Time tracking", Epic, InProgress, None, "Central", &["Central"], false),
        issue("DEMO-5", "Work log: time tracking, general", WorkLog, InProgress, Some("DEMO-100"), "Central", &["Central"], false),
        issue("DEMO-6", "Work log: maintenance", WorkLog, InProgress, None, "Central", &["Central"], false),
    ]
}

struct Item {
    source: Source,
    title: &'static str,
    from: (i8, i8),
    to: (i8, i8),
    series: Option<&'static str>,
}

const fn item(
    source: Source,
    title: &'static str,
    from: (i8, i8),
    to: (i8, i8),
    series: Option<&'static str>,
) -> Item {
    Item { source, title, from, to, series }
}

/// Monday to Friday. Small recurring meetings have a series and start out absorbed.
fn week_plan() -> Vec<Vec<Item>> {
    use Source::*;
    let standup = item(Outlook, "Daily standup", (9, 0), (9, 15), Some("standup"));
    vec![
        vec![
            standup,
            item(Git, "DEMO-97 · 4 commits", (8, 10), (11, 40), None),
            item(Git, "DEMO-101 · 2 commits", (12, 30), (15, 50), None),
            item(Outlook, "Department meeting", (13, 0), (13, 45), Some("department")),
        ],
        vec![
            item(Outlook, "Daily standup", (9, 0), (9, 15), Some("standup")),
            item(Git, "DEMO-101 · 5 commits", (8, 20), (12, 5), None),
            item(Toggl, "DEMO-101 overlap layout", (8, 25), (11, 50), None),
            item(Outlook, "Workshop: Q4 integrations DEMO-120", (12, 30), (14, 30), None),
            item(Git, "DEMO-101 · 1 commit", (14, 45), (16, 10), None),
        ],
        vec![
            item(Outlook, "Daily standup", (9, 0), (9, 15), Some("standup")),
            item(Git, "DEMO-104 · 6 commits", (8, 15), (15, 20), None),
            item(Outlook, "Pairing: customer search DEMO-130", (10, 0), (11, 30), None),
            item(Toggl, "DEMO-104 review sheet", (12, 0), (15, 0), None),
        ],
        vec![
            item(Outlook, "Daily standup", (9, 0), (9, 15), Some("standup")),
            item(Outlook, "Cross-team sync on DEMO-110", (10, 0), (11, 0), None),
            item(Git, "DEMO-110 · 3 commits", (11, 5), (15, 40), None),
            item(Outlook, "1:1", (14, 0), (14, 30), Some("one-on-one")),
        ],
        vec![
            item(Outlook, "Daily standup", (9, 0), (9, 15), Some("standup")),
            item(Git, "DEMO-104 · 2 commits", (8, 30), (12, 0), None),
            item(Outlook, "Demo and retro", (13, 0), (14, 30), Some("retro")),
        ],
    ]
}

/// Adds a day's activities. Days that haven't happened yet only have their meetings.
fn add_day(ws: &mut Workspace, date: Date, plan: &[Item], happened: bool) {
    for item in plan {
        if !happened && item.source != Source::Outlook {
            continue;
        }
        let range = TimeRange::new(
            time::at(date, item.from.0, item.from.1, &ws.tz),
            time::at(date, item.to.0, item.to.1, &ws.tz),
        );
        let id = ActivityId(ws.next_id());
        ws.activities.push(Activity {
            id,
            source: item.source,
            title: item.title.to_owned(),
            range,
            series: item.series.map(str::to_owned),
            absorbed: item.series.is_some(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::SyncOp;
    use jiff::civil::date;

    #[test]
    fn leaves_the_last_two_workdays_and_today_to_backfill() {
        let today = date(2026, 10, 28);
        let ws = workspace(today, TimeZone::UTC);
        let target = ws.settings.target;
        for day in 19..=23 {
            assert_eq!(ws.logged_on(date(2026, 10, day)), target, "Oct {day}");
        }
        for day in 26..=28 {
            assert_eq!(ws.logged_on(date(2026, 10, day)), SignedDuration::ZERO, "Oct {day}");
        }
        let plan = ws.sync_plan();
        assert_eq!(plan.len(), 2);
        assert!(plan.iter().all(|op| matches!(op, SyncOp::Update { .. })));
    }

    #[test]
    fn on_a_monday_the_end_of_last_week_is_left_to_backfill() {
        let ws = workspace(date(2026, 10, 26), TimeZone::UTC);
        let target = ws.settings.target;
        for day in 19..=21 {
            assert_eq!(ws.logged_on(date(2026, 10, day)), target, "Oct {day}");
        }
        assert_eq!(ws.logged_on(date(2026, 10, 22)), SignedDuration::ZERO);
        assert_eq!(ws.logged_on(date(2026, 10, 23)), SignedDuration::ZERO);
    }

    #[test]
    fn future_days_only_have_meetings() {
        let ws = workspace(date(2026, 10, 28), TimeZone::UTC);
        assert!(ws.activities_on(date(2026, 10, 30)).all(|a| a.source == Source::Outlook));
    }
}
