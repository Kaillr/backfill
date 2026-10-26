use jiff::Timestamp;

use crate::{Access, Calendar, Event};

pub fn access() -> Access {
    Access::Unsupported
}

pub fn request_access() -> Access {
    Access::Unsupported
}

pub fn calendars() -> Vec<Calendar> {
    Vec::new()
}

pub fn events(_start: Timestamp, _end: Timestamp, _calendar_ids: &[String]) -> Vec<Event> {
    Vec::new()
}
