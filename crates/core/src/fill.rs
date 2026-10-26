//! "Fill day": log the rest of a day's hours on its primary task.
//!
//! A day is logged as the hours actually worked, mostly on the primary task, with small
//! recurring meetings absorbed into it. So filling ignores meetings and only
//! steps around time that is already logged.

use jiff::{SignedDuration, Timestamp};

use crate::{TimeRange, time};

pub struct FillRequest<'a> {
    /// When the workday starts. Filling begins here.
    pub day_start: Timestamp,
    /// The hours the day should add up to, such as 7.5h.
    pub target: SignedDuration,
    /// Time already logged that day, in any order.
    pub logged: &'a [TimeRange],
    /// Gaps shorter than this are skipped, so filling never creates slivers.
    pub min_block: SignedDuration,
}

/// The ranges to log so the day reaches its target, earliest first. Empty when the day is
/// already full.
pub fn fill_gaps(request: &FillRequest) -> Vec<TimeRange> {
    let logged_total: SignedDuration = request.logged.iter().map(|r| r.duration()).sum();
    let mut remaining = request.target - logged_total;
    if remaining < request.min_block {
        return Vec::new();
    }

    let mut logged = request.logged.to_vec();
    logged.sort();

    let mut filled = Vec::new();
    let mut cursor = request.day_start;
    for block in &logged {
        if remaining.is_zero() {
            break;
        }
        if block.start > cursor {
            let gap = TimeRange::new(cursor, block.start);
            if gap.duration() >= request.min_block {
                let take = gap.duration().min(remaining);
                filled.push(TimeRange::starting_at(cursor, take));
                remaining -= take;
            }
        }
        cursor = cursor.max(block.end);
    }
    if remaining >= request.min_block {
        filled.push(TimeRange::starting_at(cursor, remaining));
    }
    filled
}

/// When the workday starts: the configured start, or earlier if something happened
/// before it. Snapped to the quarter hour.
pub fn day_start(configured: Timestamp, earliest_activity: Option<Timestamp>) -> Timestamp {
    let start = earliest_activity.map_or(configured, |first| first.min(configured));
    time::snap(start, 15)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::at;
    use jiff::{civil::date, tz::TimeZone};

    fn t(hour: i8, minute: i8) -> Timestamp {
        at(date(2026, 10, 26), hour, minute, &TimeZone::UTC)
    }

    fn r(from: (i8, i8), to: (i8, i8)) -> TimeRange {
        TimeRange::new(t(from.0, from.1), t(to.0, to.1))
    }

    fn fill(logged: &[TimeRange]) -> Vec<TimeRange> {
        fill_gaps(&FillRequest {
            day_start: t(8, 0),
            target: SignedDuration::from_mins(450),
            logged,
            min_block: SignedDuration::from_mins(15),
        })
    }

    #[test]
    fn an_empty_day_gets_one_block_from_the_start() {
        assert_eq!(fill(&[]), [r((8, 0), (15, 30))]);
    }

    #[test]
    fn steps_around_logged_time() {
        let workshop = r((10, 0), (12, 0));
        assert_eq!(fill(&[workshop]), [r((8, 0), (10, 0)), r((12, 0), (15, 30))]);
    }

    #[test]
    fn stops_once_the_target_is_reached() {
        let logged = [r((8, 0), (9, 0)), r((9, 30), (15, 30))];
        assert_eq!(fill(&logged), [r((9, 0), (9, 30))]);
    }

    #[test]
    fn skips_slivers() {
        let logged = [r((8, 0), (9, 0)), r((9, 10), (12, 0))];
        assert_eq!(fill(&logged), [r((12, 0), (15, 40))]);
    }

    #[test]
    fn a_full_day_needs_nothing() {
        assert!(fill(&[r((8, 0), (15, 30))]).is_empty());
        assert!(fill(&[r((8, 0), (15, 20))]).is_empty());
    }

    #[test]
    fn the_day_starts_at_the_first_activity_when_it_is_earlier() {
        assert_eq!(day_start(t(8, 0), Some(t(7, 22))), t(7, 15));
        assert_eq!(day_start(t(8, 0), Some(t(9, 0))), t(8, 0));
        assert_eq!(day_start(t(8, 0), None), t(8, 0));
    }
}
