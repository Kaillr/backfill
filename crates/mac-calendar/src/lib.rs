//! Reads events from the macOS Calendar app.
//!
//! Calendar already syncs the Outlook (Exchange) account, so reading it locally avoids
//! registering an app with Microsoft. macOS asks the user once for permission, and only
//! shows that prompt to an app bundle whose Info.plist explains the request
//! (`NSCalendarsFullAccessUsageDescription`).
//!
//! Every call blocks, so call them off the UI thread. On other platforms everything
//! reports [`Access::Unsupported`] and returns nothing.

use jiff::Timestamp;

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as imp;
#[cfg(not(target_os = "macos"))]
mod unsupported;
#[cfg(not(target_os = "macos"))]
use unsupported as imp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Granted,
    /// The user hasn't been asked yet.
    NotDetermined,
    Denied,
    /// A device policy blocks access.
    Restricted,
    /// Only adding events is allowed, which doesn't let us read anything.
    WriteOnly,
    /// Not macOS.
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalendarKind {
    /// An Exchange account, which is how Calendar syncs Outlook.
    Exchange,
    CalDav,
    Local,
    Subscribed,
    Birthdays,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Calendar {
    pub id: String,
    pub title: String,
    /// The account it belongs to, such as the work email address.
    pub account: String,
    pub kind: CalendarKind,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Unique per occurrence: a recurring meeting's occurrences share an event id, so the
    /// occurrence's start is appended.
    pub id: String,
    /// Set for occurrences of a recurring meeting, shared by all of them.
    pub series: Option<String>,
    pub calendar_id: String,
    pub title: String,
    pub start: Timestamp,
    pub end: Timestamp,
    pub all_day: bool,
    /// You declined it.
    pub declined: bool,
    /// Cancelled by the organizer, by its status. Exchange often only renames a cancelled
    /// meeting instead; see `backfill_core::activity::cancelled_title`.
    pub canceled: bool,
    /// Shown as "free": it doesn't take up your time, like a working location or a reminder.
    pub free: bool,
}

pub fn access() -> Access {
    imp::access()
}

/// Asks the user for full calendar access if they haven't been asked, and waits for the
/// answer. Returns the resulting access either way.
pub fn request_access() -> Access {
    imp::request_access()
}

pub fn calendars() -> Vec<Calendar> {
    imp::calendars()
}

/// Events overlapping `start..end` in the given calendars.
pub fn events(start: Timestamp, end: Timestamp, calendar_ids: &[String]) -> Vec<Event> {
    imp::events(start, end, calendar_ids)
}

/// Whether this process runs from an app bundle. Without one, macOS never shows the
/// permission prompt.
pub fn running_as_app() -> bool {
    std::env::current_exe()
        .is_ok_and(|exe| exe.to_string_lossy().contains(".app/Contents/MacOS/"))
}
