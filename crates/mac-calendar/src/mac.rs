use std::sync::mpsc;

use block2::RcBlock;
use jiff::Timestamp;
use objc2::runtime::Bool;
use objc2_event_kit::{
    EKAuthorizationStatus, EKCalendar, EKEntityType, EKEvent, EKEventAvailability, EKEventStatus, EKEventStore,
    EKParticipantStatus, EKSourceType,
};
use objc2_foundation::{NSArray, NSDate, NSError};

use crate::{Access, Calendar, CalendarKind, Event};

pub fn access() -> Access {
    let status = unsafe { EKEventStore::authorizationStatusForEntityType(EKEntityType::Event) };
    match status {
        EKAuthorizationStatus::FullAccess => Access::Granted,
        EKAuthorizationStatus::NotDetermined => Access::NotDetermined,
        EKAuthorizationStatus::Denied => Access::Denied,
        EKAuthorizationStatus::Restricted => Access::Restricted,
        EKAuthorizationStatus::WriteOnly => Access::WriteOnly,
        _ => Access::Denied,
    }
}

pub fn request_access() -> Access {
    if access() != Access::NotDetermined {
        return access();
    }
    let store = unsafe { EKEventStore::new() };
    let (tx, rx) = mpsc::channel();
    // EventKit calls this on a queue of its own, once the user has answered.
    let completion = RcBlock::new(move |granted: Bool, _error: *mut NSError| {
        let _ = tx.send(granted.as_bool());
    });
    unsafe { store.requestFullAccessToEventsWithCompletion(RcBlock::as_ptr(&completion)) };
    let _ = rx.recv();
    access()
}

pub fn calendars() -> Vec<Calendar> {
    if access() != Access::Granted {
        return Vec::new();
    }
    let store = unsafe { EKEventStore::new() };
    let calendars = unsafe { store.calendarsForEntityType(EKEntityType::Event) };
    calendars.iter().map(|calendar| describe(&calendar)).collect()
}

fn describe(calendar: &EKCalendar) -> Calendar {
    let source = unsafe { calendar.source() };
    let kind = match source.as_ref().map(|s| unsafe { s.sourceType() }) {
        Some(EKSourceType::Exchange) => CalendarKind::Exchange,
        Some(EKSourceType::CalDAV) => CalendarKind::CalDav,
        Some(EKSourceType::Local) => CalendarKind::Local,
        Some(EKSourceType::Subscribed) => CalendarKind::Subscribed,
        Some(EKSourceType::Birthdays) => CalendarKind::Birthdays,
        _ => CalendarKind::Other,
    };
    Calendar {
        id: unsafe { calendar.calendarIdentifier() }.to_string(),
        title: unsafe { calendar.title() }.to_string(),
        account: source.map(|s| unsafe { s.title() }.to_string()).unwrap_or_default(),
        kind,
    }
}

pub fn events(start: Timestamp, end: Timestamp, calendar_ids: &[String]) -> Vec<Event> {
    if access() != Access::Granted || calendar_ids.is_empty() {
        return Vec::new();
    }
    let store = unsafe { EKEventStore::new() };
    let all = unsafe { store.calendarsForEntityType(EKEntityType::Event) };
    let chosen: Vec<_> = all
        .iter()
        .filter(|c| calendar_ids.contains(&unsafe { c.calendarIdentifier() }.to_string()))
        .collect();
    if chosen.is_empty() {
        return Vec::new();
    }
    let chosen = NSArray::from_retained_slice(&chosen);
    let predicate = unsafe {
        store.predicateForEventsWithStartDate_endDate_calendars(
            &to_date(start),
            &to_date(end),
            Some(&chosen),
        )
    };
    let events = unsafe { store.eventsMatchingPredicate(&predicate) };
    events.iter().map(|event| convert(&event)).collect()
}

fn convert(event: &EKEvent) -> Event {
    let series_id = unsafe { event.eventIdentifier() }.map(|id| id.to_string()).unwrap_or_default();
    let start = unsafe { event.startDate() };
    let start = from_date(&start);
    let occurrence = unsafe { event.occurrenceDate() }.map_or(start, |d| from_date(&d));
    let recurring = unsafe { event.hasRecurrenceRules() || event.isDetached() };
    let declined = unsafe { event.attendees() }.is_some_and(|attendees| {
        attendees.iter().any(|a| unsafe {
            a.isCurrentUser() && a.participantStatus() == EKParticipantStatus::Declined
        })
    });
    let end = unsafe { event.endDate() };
    let calendar_id = unsafe { event.calendar() }
        .map(|c| unsafe { c.calendarIdentifier() }.to_string())
        .unwrap_or_default();
    let title = unsafe { event.title() }.to_string();
    let canceled = unsafe { event.status() } == EKEventStatus::Canceled;
    Event {
        id: format!("{series_id}@{}", occurrence.as_second()),
        series: recurring.then(|| series_id.clone()),
        calendar_id,
        title,
        start,
        end: from_date(&end),
        all_day: unsafe { event.isAllDay() },
        declined,
        canceled,
        free: unsafe { event.availability() } == EKEventAvailability::Free,
    }
}

fn to_date(ts: Timestamp) -> objc2::rc::Retained<NSDate> {
    NSDate::dateWithTimeIntervalSince1970(ts.as_millisecond() as f64 / 1000.)
}

fn from_date(date: &NSDate) -> Timestamp {
    let millis = (date.timeIntervalSince1970() * 1000.).round() as i64;
    Timestamp::from_millisecond(millis).unwrap_or(Timestamp::UNIX_EPOCH)
}
