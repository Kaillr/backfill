//! What Jira remembers about your part in issues, week by week: which were assigned to
//! you, and which you moved to another status. When backfilling an old week, this says
//! what you were on then, where an issue's status today might say otherwise.

use serde::{Deserialize, Serialize};

use crate::{IssueKey, TimeRange};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct JiraHistory {
    /// Weeks Jira has been asked about.
    weeks: Vec<TimeRange>,
    /// Issues assigned to you at some point in a week, with that week.
    assigned: Vec<(IssueKey, TimeRange)>,
    /// Issues you moved to another status in a week, with that week.
    moved: Vec<(IssueKey, TimeRange)>,
}

impl JiraHistory {
    /// Replaces what's known about `week`.
    pub fn record(&mut self, week: TimeRange, assigned: Vec<IssueKey>, moved: Vec<IssueKey>) {
        self.weeks.retain(|w| *w != week);
        self.assigned.retain(|(_, w)| *w != week);
        self.moved.retain(|(_, w)| *w != week);
        self.weeks.push(week);
        self.assigned.extend(assigned.into_iter().map(|key| (key, week)));
        self.moved.extend(moved.into_iter().map(|key| (key, week)));
    }

    /// Whether Jira has been asked about the week `focus` starts in.
    pub fn covers(&self, focus: &TimeRange) -> bool {
        self.weeks.iter().any(|w| w.contains(focus.start))
    }

    pub fn assigned(&self, key: &IssueKey, focus: &TimeRange) -> bool {
        self.assigned.iter().any(|(k, w)| k == key && w.contains(focus.start))
    }

    pub fn moved(&self, key: &IssueKey, focus: &TimeRange) -> bool {
        self.moved.iter().any(|(k, w)| k == key && w.contains(focus.start))
    }
}

#[cfg(test)]
mod tests {
    use jiff::{civil::date, tz::TimeZone};

    use super::*;
    use crate::time::at;

    fn week(day: i8) -> TimeRange {
        TimeRange::new(at(date(2026, 10, day), 0, 0, &TimeZone::UTC), at(date(2026, 10, day + 7), 0, 0, &TimeZone::UTC))
    }

    fn hour(day: i8) -> TimeRange {
        TimeRange::new(at(date(2026, 10, day), 9, 0, &TimeZone::UTC), at(date(2026, 10, day), 10, 0, &TimeZone::UTC))
    }

    #[test]
    fn answers_for_the_week_a_time_falls_in_and_a_new_answer_replaces_the_old() {
        let key = IssueKey::parse("AB-1").unwrap();
        let mut history = JiraHistory::default();
        history.record(week(19), vec![key.clone()], Vec::new());
        assert!(history.covers(&hour(21)) && history.assigned(&key, &hour(21)) && !history.moved(&key, &hour(21)));
        assert!(!history.covers(&hour(27)));
        history.record(week(19), Vec::new(), vec![key.clone()]);
        assert!(!history.assigned(&key, &hour(21)) && history.moved(&key, &hour(21)));
    }
}
