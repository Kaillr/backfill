use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};

/// A half-open span of time, `[start, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Timestamp,
    pub end: Timestamp,
}

impl TimeRange {
    /// Builds a range, swapping the ends if they arrive backwards (as they do when a drag
    /// moves upwards).
    pub fn new(a: Timestamp, b: Timestamp) -> Self {
        if a <= b {
            Self { start: a, end: b }
        } else {
            Self { start: b, end: a }
        }
    }

    pub fn starting_at(start: Timestamp, duration: SignedDuration) -> Self {
        Self::new(start, start + duration)
    }

    pub fn duration(&self) -> SignedDuration {
        self.end.duration_since(self.start)
    }

    pub fn minutes(&self) -> i64 {
        self.duration().as_mins()
    }

    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub fn overlaps(&self, other: &TimeRange) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn contains(&self, at: Timestamp) -> bool {
        self.start <= at && at < self.end
    }

    /// The part of `self` that falls inside `other`, if any.
    pub fn clip(&self, other: &TimeRange) -> Option<TimeRange> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        (start < end).then_some(TimeRange { start, end })
    }

    pub fn shifted(&self, by: SignedDuration) -> TimeRange {
        TimeRange {
            start: self.start + by,
            end: self.end + by,
        }
    }
}

/// The local calendar day `ts` falls on.
pub fn local_date(ts: Timestamp, tz: &TimeZone) -> Date {
    ts.to_zoned(tz.clone()).date()
}

/// The instant at `hour:minute` local time on `date`.
pub fn at(date: Date, hour: i8, minute: i8, tz: &TimeZone) -> Timestamp {
    date.at(hour, minute, 0, 0)
        .to_zoned(tz.clone())
        .expect("local time is representable")
        .timestamp()
}

/// The whole local day, midnight to midnight.
pub fn day_range(date: Date, tz: &TimeZone) -> TimeRange {
    let start = at(date, 0, 0, tz);
    let next = date.tomorrow().expect("date in range");
    TimeRange::new(start, at(next, 0, 0, tz))
}

/// Minutes from local midnight of `date` to `ts`. Negative before the day starts.
pub fn minutes_into_day(ts: Timestamp, date: Date, tz: &TimeZone) -> f32 {
    let midnight = at(date, 0, 0, tz);
    ts.duration_since(midnight).as_secs() as f32 / 60.0
}

/// The instant `minutes` after local midnight of `date`.
pub fn from_minutes_into_day(minutes: f32, date: Date, tz: &TimeZone) -> Timestamp {
    at(date, 0, 0, tz) + SignedDuration::from_secs((minutes * 60.0).round() as i64)
}

/// Rounds to the nearest multiple of `step_minutes` on the wall clock.
///
/// Every real UTC offset is a whole number of quarter hours, so rounding the epoch
/// seconds lands on the same wall-clock grid in any time zone for steps up to 15 minutes.
pub fn snap(ts: Timestamp, step_minutes: i64) -> Timestamp {
    let step = step_minutes * 60;
    let secs = ts.as_second();
    let snapped = (secs + step / 2).div_euclid(step) * step;
    Timestamp::from_second(snapped).expect("snapped timestamp in range")
}

/// The Monday of the week `date` belongs to.
pub fn week_start(date: Date) -> Date {
    let offset = date.weekday().to_monday_zero_offset() as i64;
    date.checked_sub(jiff::Span::new().days(offset))
        .expect("date in range")
}

pub fn add_days(date: Date, days: i64) -> Date {
    date.checked_add(jiff::Span::new().days(days))
        .expect("date in range")
}

/// Formats a duration the way people say it: `7.5h`, `45m`, `2h`.
pub fn format_hours(duration: SignedDuration) -> String {
    let mins = duration.as_mins();
    if mins.abs() < 60 {
        return format!("{mins}m");
    }
    if mins % 60 == 0 {
        return format!("{}h", mins / 60);
    }
    let hours = format!("{:.2}", mins as f64 / 60.0);
    format!("{}h", hours.trim_end_matches('0'))
}

/// Reads hours the way people type them into a timesheet: `7.5`, `7,5`, `1h30`, `1h 30m`,
/// `90m`. A bare number is hours. Returns `None` for anything else, or for more than a day.
pub fn parse_hours(text: &str) -> Option<SignedDuration> {
    let text = text.trim().to_lowercase().replace(',', ".").replace(' ', "");
    if text.is_empty() {
        return None;
    }
    let minutes = if let Some((hours, rest)) = text.split_once('h') {
        let hours: f64 = if hours.is_empty() { 0. } else { hours.parse().ok()? };
        let rest = rest.trim_end_matches('m');
        let mins: f64 = if rest.is_empty() { 0. } else { rest.parse().ok()? };
        hours * 60. + mins
    } else if let Some(mins) = text.strip_suffix('m') {
        mins.parse::<f64>().ok()?
    } else {
        text.parse::<f64>().ok()? * 60.
    };
    (minutes.is_finite() && (0. ..=24. * 60.).contains(&minutes))
        .then(|| SignedDuration::from_mins(minutes.round() as i64))
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    const TZ: TimeZone = TimeZone::UTC;

    #[test]
    fn range_orders_its_ends() {
        let a = at(date(2026, 10, 26), 10, 0, &TZ);
        let b = at(date(2026, 10, 26), 9, 0, &TZ);
        let range = TimeRange::new(a, b);
        assert_eq!(range.start, b);
        assert_eq!(range.minutes(), 60);
    }

    #[test]
    fn touching_ranges_do_not_overlap() {
        let day = date(2026, 10, 26);
        let morning = TimeRange::new(at(day, 9, 0, &TZ), at(day, 10, 0, &TZ));
        let next = TimeRange::new(at(day, 10, 0, &TZ), at(day, 11, 0, &TZ));
        assert!(!morning.overlaps(&next));
        assert!(morning.overlaps(&TimeRange::new(at(day, 9, 59, &TZ), at(day, 11, 0, &TZ))));
    }

    #[test]
    fn snaps_to_quarter_hours() {
        let day = date(2026, 10, 26);
        assert_eq!(snap(at(day, 9, 7, &TZ), 15), at(day, 9, 0, &TZ));
        assert_eq!(snap(at(day, 9, 8, &TZ), 15), at(day, 9, 15, &TZ));
    }

    #[test]
    fn snapping_follows_the_local_clock_in_oslo() {
        let oslo = TimeZone::get("Europe/Oslo").unwrap();
        let day = date(2026, 10, 26);
        assert_eq!(snap(at(day, 9, 52, &oslo), 15), at(day, 9, 45, &oslo));
    }

    #[test]
    fn weeks_start_on_monday() {
        assert_eq!(week_start(date(2026, 10, 29)), date(2026, 10, 26));
        assert_eq!(week_start(date(2026, 10, 26)), date(2026, 10, 26));
        assert_eq!(week_start(date(2026, 11, 1)), date(2026, 10, 26));
    }

    #[test]
    fn parses_hours_as_typed() {
        let mins = |s: &str| parse_hours(s).map(|d| d.as_mins());
        assert_eq!(mins("7.5"), Some(450));
        assert_eq!(mins("7,5"), Some(450));
        assert_eq!(mins("2"), Some(120));
        assert_eq!(mins("1h30"), Some(90));
        assert_eq!(mins("1h 30m"), Some(90));
        assert_eq!(mins("45m"), Some(45));
        assert_eq!(mins("h"), Some(0));
        assert_eq!(mins(""), None);
        assert_eq!(mins("abc"), None);
        assert_eq!(mins("25"), None);
    }

    #[test]
    fn formats_hours_like_people_say_them() {
        assert_eq!(format_hours(SignedDuration::from_mins(450)), "7.5h");
        assert_eq!(format_hours(SignedDuration::from_mins(120)), "2h");
        assert_eq!(format_hours(SignedDuration::from_mins(45)), "45m");
        assert_eq!(format_hours(SignedDuration::from_mins(75)), "1.25h");
    }
}
