use jiff::{
    SignedDuration,
    civil::{Date, Time, Weekday},
    tz::TimeZone,
};
use serde::{Deserialize, Serialize};

use crate::{
    IssueKey, TimeRange,
    issue::find_issue_keys,
    time::{add_days, at, day_range, local_date},
};

/// Where an activity came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    Outlook,
    Toggl,
    /// A commit: a moment, never a length of time.
    Git,
    /// A branch checked out, from switching to it until switching away. Only used to rank
    /// suggestions: it doesn't say how long you worked, so it's never drawn.
    Branch,
    Manual,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Outlook => "Outlook",
            Source::Toggl => "Toggl",
            Source::Git => "Git",
            Source::Branch => "Branch",
            Source::Manual => "Manual",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ActivityId(pub u64);

/// Something that happened: a meeting, a timer, a stretch on a branch.
///
/// Activities never go to Jira themselves. Linking one creates a worklog that remembers
/// it as its origin, so "is this linked?" is answered by the worklogs, not stored here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub id: ActivityId,
    pub source: Source,
    pub title: String,
    pub range: TimeRange,
    /// Recurring meetings share a series, so a choice made for one can apply to all.
    pub series: Option<String>,
    /// Small recurring meetings are absorbed into the day's primary task instead of
    /// being logged on their own.
    pub absorbed: bool,
}

/// Whether a title says the meeting was cancelled. Exchange renames a cancelled meeting
/// in its organizer's language, as in "Canceled: Standup", "Avlyst: Standup" or
/// "Avbruten: Standup", and often leaves it in attendees' calendars without marking it
/// cancelled. Toggl entries made from it keep that title.
pub fn cancelled_title(title: &str) -> bool {
    const CANCELLED: [&str; 20] = [
        "canceled", "cancelled", "avlyst", "avbrutt", "aflyst", "avbruten", "inställt", "inställd", "avbokad",
        "abgesagt", "peruutettu", "peruttu", "geannuleerd", "annulé", "annulée", "annullato", "cancelado", "cancelada",
        "anulowano", "odwołane",
    ];
    title.split_once(':').is_some_and(|(head, _)| CANCELLED.contains(&head.trim().to_lowercase().as_str()))
}

impl Activity {
    /// The issue this activity names, if its title mentions one.
    pub fn issue_hint(&self) -> Option<IssueKey> {
        find_issue_keys(&self.title).into_iter().next()
    }

    /// The time this activity takes on each day it touches, earliest first. `workday`
    /// is when the working day usually starts and ends.
    ///
    /// A meeting over several days that sets its daily hours, like a gathering from
    /// Wednesday 09:00 to Thursday 16:30, is those hours on each day: 09:00 to 16:30 on
    /// both. One that runs on from one day into the next, like a hackathon from Thursday
    /// 09:00 to Friday 13:00, is the working day on each: from 09:00 to the end of
    /// Thursday's, and from the start of Friday's to 13:00. Anything else past midnight,
    /// like a timer left running, is split at midnight. Weekends in between are left out,
    /// since the calendar shows Monday to Friday.
    pub fn day_parts(&self, tz: &TimeZone, workday: (Time, Time)) -> Vec<TimeRange> {
        let first = local_date(self.range.start, tz);
        let last = local_date(self.range.end - SignedDuration::from_nanos(1), tz);
        if first >= last {
            return vec![self.range];
        }
        let mut days = Vec::new();
        let mut date = first;
        while date <= last {
            if !matches!(date.weekday(), Weekday::Saturday | Weekday::Sunday) {
                days.push(date);
            }
            date = add_days(date, 1);
        }
        let at_time = |date: Date, time: Time| at(date, time.hour(), time.minute(), tz);
        let split_at_midnight = || days.iter().filter_map(|d| self.range.clip(&day_range(*d, tz))).collect::<Vec<_>>();
        if self.source != Source::Outlook {
            return split_at_midnight();
        }
        let (from, to) = (self.range.start.to_zoned(tz.clone()).time(), self.range.end.to_zoned(tz.clone()).time());
        if to > from && to.duration_since(from) >= SignedDuration::from_hours(6) {
            return days.iter().map(|d| TimeRange::new(at_time(*d, from), at_time(*d, to))).collect();
        }
        let (open, close) = workday;
        let parts: Vec<TimeRange> = days
            .iter()
            .filter_map(|d| {
                let start = if *d == first { open.min(from) } else { open };
                let end = if *d == last { close.max(to) } else { close };
                TimeRange::new(at_time(*d, start), at_time(*d, end)).clip(&self.range)
            })
            .collect();
        // Nothing inside working hours, as for a meeting through the night.
        if parts.is_empty() { split_at_midnight() } else { parts }
    }

    /// Whether this is a Toggl entry made from `meeting`: the same title, mostly at the
    /// same time. Toggl can turn calendar events into time entries, and the meeting
    /// itself is already in the calendar.
    pub fn copies(&self, meeting: &Activity) -> bool {
        let same_title = |a: &str, b: &str| a.split_whitespace().map(str::to_lowercase).eq(b.split_whitespace().map(str::to_lowercase));
        let shorter = self.range.duration().min(meeting.range.duration());
        let shared = self.range.clip(&meeting.range).map_or(SignedDuration::ZERO, |r| r.duration());
        self.source == Source::Toggl
            && meeting.source == Source::Outlook
            && same_title(&self.title, &meeting.title)
            && shared * 2 >= shorter
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_meetings_renamed_as_cancelled() {
        assert!(cancelled_title("Avlyst: Team standup"));
        assert!(cancelled_title("Avbrutt: Quarterly review"));
        assert!(cancelled_title("Avbruten: Team meeting | Planning"));
        assert!(cancelled_title("Annulé : Revue"));
        assert!(cancelled_title("Canceled: Planning"));
        assert!(cancelled_title("  CANCELLED: Planning"));
        assert!(!cancelled_title("Planning: what got cancelled"));
        assert!(!cancelled_title("Avlysning av lisens"));
    }

    fn activity(source: Source, from: &str, to: &str) -> Activity {
        let range = TimeRange::new(from.parse().unwrap(), to.parse().unwrap());
        Activity { id: ActivityId(1), source, title: "Gathering".into(), range, series: None, absorbed: false }
    }

    fn parts(activity: &Activity) -> Vec<String> {
        let workday = (jiff::civil::time(8, 0, 0, 0), jiff::civil::time(15, 30, 0, 0));
        activity.day_parts(&TimeZone::UTC, workday).iter().map(|r| format!("{} {}", r.start, r.end)).collect()
    }

    #[test]
    fn a_meeting_over_several_days_is_its_daily_hours_on_each_weekday() {
        // Wednesday 21 to Thursday 22 October.
        let gathering = activity(Source::Outlook, "2026-10-21T09:00:00Z", "2026-10-22T16:30:00Z");
        assert_eq!(parts(&gathering), ["2026-10-21T09:00:00Z 2026-10-21T16:30:00Z", "2026-10-22T09:00:00Z 2026-10-22T16:30:00Z"]);
        // Friday to Monday skips the weekend.
        let long = activity(Source::Outlook, "2026-10-23T09:00:00Z", "2026-10-26T16:00:00Z");
        assert_eq!(parts(&long), ["2026-10-23T09:00:00Z 2026-10-23T16:00:00Z", "2026-10-26T09:00:00Z 2026-10-26T16:00:00Z"]);
    }

    #[test]
    fn a_meeting_that_runs_on_into_the_next_day_is_the_working_day_on_each() {
        // Thursday 09:00 to Friday 13:00, with an 08:00 to 15:30 working day.
        let hackathon = activity(Source::Outlook, "2026-10-15T09:00:00Z", "2026-10-16T13:00:00Z");
        assert_eq!(parts(&hackathon), ["2026-10-15T09:00:00Z 2026-10-15T15:30:00Z", "2026-10-16T08:00:00Z 2026-10-16T13:00:00Z"]);
        // Ending after the working day still counts to its end.
        let late = activity(Source::Outlook, "2026-10-15T13:00:00Z", "2026-10-16T17:00:00Z");
        assert_eq!(parts(&late), ["2026-10-15T13:00:00Z 2026-10-15T15:30:00Z", "2026-10-16T08:00:00Z 2026-10-16T17:00:00Z"]);
    }

    #[test]
    fn anything_else_past_midnight_is_split_there_and_a_day_stays_whole() {
        let late = activity(Source::Toggl, "2026-10-21T22:00:00Z", "2026-10-22T02:00:00Z");
        assert_eq!(parts(&late), ["2026-10-21T22:00:00Z 2026-10-22T00:00:00Z", "2026-10-22T00:00:00Z 2026-10-22T02:00:00Z"]);
        let overnight = activity(Source::Outlook, "2026-10-21T20:00:00Z", "2026-10-22T06:00:00Z");
        assert_eq!(parts(&overnight).len(), 2);
        let ordinary = activity(Source::Outlook, "2026-10-21T09:00:00Z", "2026-10-21T10:00:00Z");
        assert_eq!(parts(&ordinary), ["2026-10-21T09:00:00Z 2026-10-21T10:00:00Z"]);
        let to_midnight = activity(Source::Toggl, "2026-10-21T22:00:00Z", "2026-10-22T00:00:00Z");
        assert_eq!(parts(&to_midnight).len(), 1);
    }
}
