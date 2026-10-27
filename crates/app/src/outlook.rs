//! Outlook meetings, read from the macOS Calendar app, which syncs the Outlook account.

use backfill_core::{Activity, ActivityId, Source, TimeRange, activity::cancelled_title};
use backfill_mac_calendar::{self as mac, Access, Calendar, CalendarKind};

use crate::{settings::CalendarFix, sources::stable_id};

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    /// Waiting for the first load.
    Starting,
    /// Not macOS.
    Unsupported,
    /// Started outside the app bundle, so macOS won't show the permission prompt.
    NotBundled,
    /// macOS hasn't asked yet, or its question never reached the user. backfill isn't
    /// listed under Calendars in Privacy & Security until it has.
    NotAsked,
    /// Calendar access was refused.
    Denied,
    /// A policy, such as a managed work Mac's, doesn't let apps read the calendar.
    Restricted,
    /// Reading these calendars.
    Connected { calendars: Vec<String> },
}

impl Status {
    /// What the user can do to let backfill read the calendar, if anything.
    pub fn fix(&self) -> Option<CalendarFix> {
        match self {
            Status::NotAsked => Some(CalendarFix::Ask),
            Status::Denied => Some(CalendarFix::OpenPrivacy),
            _ => None,
        }
    }
}

pub struct Load {
    pub window: TimeRange,
    pub status: Status,
    /// Present only when connected; replaces the meetings in `window`.
    pub activities: Option<Vec<Activity>>,
    /// Meetings in `window` that were cancelled or declined.
    pub skipped: Vec<Activity>,
}

/// Reads the meetings in `window`. With `ask`, macOS asks for calendar access first if
/// the user hasn't been asked yet. Blocks, so call it off the UI thread.
pub fn load(window: TimeRange, ask: bool) -> Load {
    let unavailable = |status| Load { window, status, activities: None, skipped: Vec::new() };
    let access = match mac::access() {
        Access::Unsupported => return unavailable(Status::Unsupported),
        Access::NotDetermined if !mac::running_as_app() => return unavailable(Status::NotBundled),
        Access::NotDetermined if ask => mac::request_access(),
        access => access,
    };
    match access {
        Access::Granted => {}
        // Still undecided after asking means the question never got to the user.
        Access::NotDetermined => return unavailable(Status::NotAsked),
        Access::Restricted => return unavailable(Status::Restricted),
        _ => return unavailable(Status::Denied),
    }

    let calendars = mac::calendars();
    let chosen = choose(&calendars);
    let ids: Vec<String> = chosen.iter().map(|c| c.id.clone()).collect();
    let events = mac::events(window.start, window.end, &ids);
    let meeting = |e: mac::Event| Activity {
        id: ActivityId(stable_id(&e.id)),
        source: Source::Outlook,
        title: if e.title.trim().is_empty() { "(No title)".to_owned() } else { e.title },
        range: TimeRange::new(e.start, e.end),
        series: e.series,
        absorbed: false,
    };
    let (skipped, kept): (Vec<mac::Event>, Vec<mac::Event>) = events
        .into_iter()
        .filter(|e| !e.all_day && !e.free && e.end > e.start)
        .partition(|e| e.declined || e.canceled || cancelled_title(&e.title));
    let activities: Vec<Activity> = kept.into_iter().map(meeting).collect();
    let skipped: Vec<Activity> = skipped.into_iter().map(meeting).collect();
    // Counts only: meeting titles are company information and stay out of logs.
    eprintln!(
        "calendar: {} of {} calendars, {} meetings, {} cancelled or declined, between {} and {}",
        chosen.len(),
        calendars.len(),
        activities.len(),
        skipped.len(),
        window.start,
        window.end
    );
    let titles = chosen.iter().map(|c| c.title.clone()).collect();
    Load { window, status: Status::Connected { calendars: titles }, activities: Some(activities), skipped }
}

/// The Outlook calendars: those from Exchange accounts. Without any, every calendar except
/// birthdays and subscriptions (holidays and the like).
fn choose(calendars: &[Calendar]) -> Vec<&Calendar> {
    let exchange: Vec<&Calendar> =
        calendars.iter().filter(|c| c.kind == CalendarKind::Exchange).collect();
    if !exchange.is_empty() {
        return exchange;
    }
    calendars
        .iter()
        .filter(|c| !matches!(c.kind, CalendarKind::Birthdays | CalendarKind::Subscribed))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calendar(title: &str, kind: CalendarKind) -> Calendar {
        Calendar { id: title.into(), title: title.into(), account: String::new(), kind }
    }

    #[test]
    fn prefers_exchange_calendars() {
        let all = [
            calendar("Calendar", CalendarKind::Exchange),
            calendar("Home", CalendarKind::CalDav),
            calendar("Holidays", CalendarKind::Subscribed),
        ];
        let titles: Vec<&str> = choose(&all).iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Calendar"]);
    }

    #[test]
    fn without_exchange_skips_birthdays_and_subscriptions() {
        let all = [
            calendar("Work", CalendarKind::CalDav),
            calendar("Birthdays", CalendarKind::Birthdays),
            calendar("Holidays", CalendarKind::Subscribed),
        ];
        let titles: Vec<&str> = choose(&all).iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Work"]);
    }

    #[test]
    fn ids_are_stable_and_stay_out_of_the_workspace_range() {
        assert_eq!(stable_id("abc@1"), stable_id("abc@1"));
        assert_ne!(stable_id("abc@1"), stable_id("abc@2"));
        assert!(stable_id("abc@1") >= 1 << 63);
    }
}
