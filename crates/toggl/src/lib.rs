//! Toggl Track time entries, cached so the app almost never has to ask.
//!
//! Toggl allows 30 requests an hour for your own data, and its time-entry endpoint only
//! reaches back about three months. So the cache is filled with one request for those three
//! months, and kept current with one request for whatever changed since the last sync.
//! Moving between weeks never touches the network. Every request is counted, syncs stay
//! well under the hourly limit, and a rate-limit answer from Toggl pauses syncing for as
//! long as Toggl asks.

use std::collections::BTreeMap;

use backfill_http::{Method, Request, Response, Transport, basic_auth};
use jiff::{SignedDuration, Timestamp, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};

const API: &str = "https://api.track.toggl.com/api/v9";
/// How far back Toggl serves time entries.
const HISTORY_DAYS: i64 = 89;
/// Automatic syncs are at least this far apart. A sync you ask for ignores it.
const MIN_INTERVAL: SignedDuration = SignedDuration::from_mins(10);
/// Requests allowed per rolling hour. Toggl allows 30; the rest is headroom for
/// connection tests and syncs you ask for.
const HOURLY_BUDGET: usize = 24;
/// How long to pause when Toggl says to slow down without saying for how long.
const DEFAULT_BACKOFF: SignedDuration = SignedDuration::from_mins(30);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: i64,
    pub description: String,
    pub start: Timestamp,
    /// `None` while the timer is running.
    pub stop: Option<Timestamp>,
    pub project_id: Option<i64>,
    pub tags: Vec<String>,
}

/// Who the token belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Me {
    pub name: String,
    pub email: String,
}

/// Everything kept between runs.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Cache {
    pub entries: BTreeMap<i64, Entry>,
    /// When the last successful sync was sent. The next one asks for changes since then.
    pub synced_at: Option<Timestamp>,
    /// When requests were sent, for the hourly budget.
    pub requests: Vec<Timestamp>,
    /// No requests before this, because Toggl said to wait.
    pub paused_until: Option<Timestamp>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Sync {
    /// Asked Toggl; `changed` entries were added, changed or removed.
    Synced { changed: usize },
    /// Synced recently enough already.
    Fresh,
    /// Waiting so as not to use up the hourly limit.
    SavingRequests { until: Timestamp },
    /// Toggl asked to wait.
    Paused { until: Timestamp },
    Failed(Error),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Error {
    /// The token was refused.
    BadToken,
    /// No answer at all, such as when offline.
    Unreachable(String),
    /// Anything else Toggl answered with.
    Unexpected { status: u16, body: String },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::BadToken => f.write_str("Toggl didn't accept the API token"),
            Error::Unreachable(reason) => write!(f, "couldn't reach Toggl: {reason}"),
            Error::Unexpected { status, body } => write!(f, "Toggl answered {status}: {}", body.chars().take(160).collect::<String>()),
        }
    }
}

pub struct Client<'a> {
    transport: &'a dyn Transport,
    token: String,
}

impl<'a> Client<'a> {
    pub fn new(transport: &'a dyn Transport, token: impl Into<String>) -> Self {
        Self { transport, token: token.into() }
    }

    fn get(&self, path: &str) -> Result<Response, Error> {
        let request = Request::new(Method::Get, format!("{API}{path}")).header("Authorization", basic_auth(&self.token, "api_token"));
        self.transport.send(request).map_err(Error::Unreachable)
    }

    /// Checks the token. Counts against the hourly limit like any request.
    pub fn me(&self, cache: &mut Cache, now: Timestamp) -> Result<Me, Error> {
        cache.requests.push(now);
        let response = self.get("/me")?;
        check(&response)?;
        let me: MeJson = parse(&response)?;
        Ok(Me { name: me.fullname.unwrap_or_default(), email: me.email.unwrap_or_default() })
    }

    /// Brings the cache up to date with at most one request. `asked` is a sync the user
    /// asked for, which skips the minimum interval but still respects the hourly budget
    /// and any pause Toggl asked for.
    pub fn sync(&self, cache: &mut Cache, now: Timestamp, tz: &TimeZone, asked: bool) -> Sync {
        let hour_ago = now - SignedDuration::from_hours(1);
        cache.requests.retain(|t| *t > hour_ago);
        if let Some(until) = cache.paused_until.filter(|until| *until > now) {
            return Sync::Paused { until };
        }
        if !asked && cache.synced_at.is_some_and(|at| now.duration_since(at) < MIN_INTERVAL) {
            return Sync::Fresh;
        }
        if cache.requests.len() >= HOURLY_BUDGET {
            let until = cache.requests[cache.requests.len() - HOURLY_BUDGET] + SignedDuration::from_hours(1);
            return Sync::SavingRequests { until };
        }

        // Changes since the last sync, unless that's too long ago for Toggl to answer.
        let since = cache
            .synced_at
            .filter(|at| now.duration_since(*at) < SignedDuration::from_hours(24 * (HISTORY_DAYS - 2)))
            .map(|at| at - SignedDuration::from_mins(1));
        let path = match since {
            Some(since) => format!("/me/time_entries?since={}", since.as_second()),
            None => {
                let today = now.to_zoned(tz.clone()).date();
                format!("/me/time_entries?start_date={}&end_date={}", days(today, -HISTORY_DAYS), days(today, 2))
            }
        };
        cache.requests.push(now);
        let response = match self.get(&path) {
            Ok(response) => response,
            Err(error) => return Sync::Failed(error),
        };
        if matches!(response.status, 402 | 429) {
            let wait = response.retry_after.map_or(DEFAULT_BACKOFF, |s| SignedDuration::from_secs(s as i64));
            let until = now + wait;
            cache.paused_until = Some(until);
            return Sync::Paused { until };
        }
        if let Err(error) = check(&response) {
            return Sync::Failed(error);
        }
        let entries: Vec<EntryJson> = match parse(&response) {
            Ok(entries) => entries,
            Err(error) => return Sync::Failed(error),
        };
        let changed = entries.len();
        if since.is_none() {
            cache.entries.clear();
        }
        for json in entries {
            if json.server_deleted_at.is_some() {
                cache.entries.remove(&json.id);
            } else if let Some(entry) = json.into_entry() {
                cache.entries.insert(entry.id, entry);
            }
        }
        cache.synced_at = Some(now);
        cache.paused_until = None;
        Sync::Synced { changed }
    }
}

impl Cache {
    /// Entries overlapping `start..end`. A running timer counts up to `now`.
    pub fn between(&self, start: Timestamp, end: Timestamp) -> impl Iterator<Item = &Entry> {
        self.entries.values().filter(move |e| e.start < end && e.stop.is_none_or(|stop| stop > start))
    }

    /// Requests left this hour, as of `now`.
    pub fn requests_left(&self, now: Timestamp) -> usize {
        let hour_ago = now - SignedDuration::from_hours(1);
        HOURLY_BUDGET.saturating_sub(self.requests.iter().filter(|t| **t > hour_ago).count())
    }
}

fn days(date: Date, offset: i64) -> Date {
    date.checked_add(jiff::Span::new().days(offset)).expect("date in range")
}

fn check(response: &Response) -> Result<(), Error> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(Error::BadToken),
        status => Err(Error::Unexpected { status, body: response.body.clone() }),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(response: &Response) -> Result<T, Error> {
    serde_json::from_str(&response.body).map_err(|e| Error::Unexpected { status: response.status, body: e.to_string() })
}

#[derive(Deserialize)]
struct MeJson {
    fullname: Option<String>,
    email: Option<String>,
}

#[derive(Deserialize)]
struct EntryJson {
    id: i64,
    description: Option<String>,
    start: Option<Timestamp>,
    stop: Option<Timestamp>,
    project_id: Option<i64>,
    tags: Option<Vec<String>>,
    server_deleted_at: Option<String>,
}

impl EntryJson {
    fn into_entry(self) -> Option<Entry> {
        Some(Entry {
            id: self.id,
            description: self.description.unwrap_or_default(),
            start: self.start?,
            stop: self.stop,
            project_id: self.project_id,
            tags: self.tags.unwrap_or_default(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backfill_http::fake::Fake;

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    const NOW: &str = "2026-10-28T12:00:00Z";

    const THREE_MONTHS: &str = r#"[
        {"id": 1, "description": "APP-123 review", "start": "2026-10-26T08:25:00+00:00",
         "stop": "2026-10-26T11:50:00+00:00", "duration": 12300, "project_id": null,
         "tags": [], "server_deleted_at": null},
        {"id": 2, "description": null, "start": "2026-10-27T12:00:00Z", "stop": null,
         "duration": -1, "tags": null}
    ]"#;

    fn synced() -> (Fake, Cache) {
        let fake = Fake::new();
        fake.respond(Method::Get, "start_date=", 200, THREE_MONTHS);
        let mut cache = Cache::default();
        let outcome = Client::new(&fake, "token").sync(&mut cache, at(NOW), &TimeZone::UTC, false);
        assert_eq!(outcome, Sync::Synced { changed: 2 });
        (fake, cache)
    }

    #[test]
    fn the_first_sync_fetches_three_months_in_one_request() {
        let (fake, cache) = synced();
        assert_eq!(fake.urls(), ["https://api.track.toggl.com/api/v9/me/time_entries?start_date=2026-07-31&end_date=2026-10-30"]);
        assert_eq!(cache.entries.len(), 2);
        assert_eq!(cache.entries[&1].description, "APP-123 review");
        assert_eq!(cache.entries[&2].stop, None, "still running");
        let auth = &fake.requests.lock().unwrap()[0].headers[0];
        assert_eq!(auth.1, basic_auth("token", "api_token"));
    }

    #[test]
    fn later_syncs_ask_only_for_changes_and_apply_deletions() {
        let (fake, mut cache) = synced();
        fake.respond(
            Method::Get,
            "since=",
            200,
            r#"[{"id": 1, "server_deleted_at": "2026-10-28T12:05:00Z", "start": "2026-10-26T08:25:00Z"},
                {"id": 3, "description": "APP-124", "start": "2026-10-28T09:00:00Z", "stop": "2026-10-28T10:00:00Z"}]"#,
        );
        let later = at("2026-10-28T12:30:00Z");
        let outcome = Client::new(&fake, "token").sync(&mut cache, later, &TimeZone::UTC, false);
        assert_eq!(outcome, Sync::Synced { changed: 2 });
        assert!(fake.urls()[1].ends_with(&format!("since={}", at(NOW).as_second() - 60)));
        assert_eq!(cache.entries.keys().copied().collect::<Vec<_>>(), [2, 3]);
    }

    #[test]
    fn a_recent_sync_is_fresh_enough_unless_asked() {
        let (fake, mut cache) = synced();
        let soon = at("2026-10-28T12:05:00Z");
        assert_eq!(Client::new(&fake, "token").sync(&mut cache, soon, &TimeZone::UTC, false), Sync::Fresh);
        assert_eq!(fake.urls().len(), 1);
        fake.respond(Method::Get, "since=", 200, "[]");
        assert_eq!(Client::new(&fake, "token").sync(&mut cache, soon, &TimeZone::UTC, true), Sync::Synced { changed: 0 });
    }

    #[test]
    fn syncs_stop_before_the_hourly_limit() {
        let (fake, mut cache) = synced();
        let now = at(NOW);
        cache.requests = (0..HOURLY_BUDGET as i64).map(|i| now - SignedDuration::from_mins(50 - i)).collect();
        let outcome = Client::new(&fake, "token").sync(&mut cache, now, &TimeZone::UTC, true);
        assert_eq!(outcome, Sync::SavingRequests { until: now + SignedDuration::from_mins(10) });
        assert_eq!(fake.urls().len(), 1);
        assert_eq!(cache.requests_left(now), 0);
        // The oldest request leaves the hour ten minutes from now, freeing one.
        assert_eq!(cache.requests_left(now + SignedDuration::from_secs(10 * 60 + 30)), 1);
    }

    #[test]
    fn a_rate_limit_answer_pauses_syncing_for_as_long_as_toggl_asks() {
        let (fake, mut cache) = synced();
        fake.respond_with(Method::Get, "since=", Response { status: 429, body: String::new(), retry_after: Some(120) });
        let now = at("2026-10-28T13:00:00Z");
        let client = Client::new(&fake, "token");
        assert_eq!(client.sync(&mut cache, now, &TimeZone::UTC, true), Sync::Paused { until: now + SignedDuration::from_secs(120) });
        let soon = now + SignedDuration::from_secs(60);
        assert_eq!(client.sync(&mut cache, soon, &TimeZone::UTC, true), Sync::Paused { until: now + SignedDuration::from_secs(120) });
        assert_eq!(fake.urls().len(), 2, "no request while paused");
    }

    #[test]
    fn a_refused_token_is_reported_plainly() {
        let fake = Fake::new();
        fake.respond(Method::Get, "start_date=", 403, "Incorrect username and/or password");
        let mut cache = Cache::default();
        let outcome = Client::new(&fake, "wrong").sync(&mut cache, at(NOW), &TimeZone::UTC, false);
        assert_eq!(outcome, Sync::Failed(Error::BadToken));
        assert!(cache.synced_at.is_none());
    }

    #[test]
    fn a_sync_too_old_for_since_starts_over() {
        let (fake, mut cache) = synced();
        fake.respond(Method::Get, "start_date=", 200, "[]");
        let much_later = at(NOW) + SignedDuration::from_hours(24 * 120);
        Client::new(&fake, "token").sync(&mut cache, much_later, &TimeZone::UTC, false);
        assert!(fake.urls()[1].contains("start_date="));
        assert!(cache.entries.is_empty());
    }

    #[test]
    fn entries_are_found_by_time_and_a_running_one_counts_as_open() {
        let (_, cache) = synced();
        let ids = |a: &str, b: &str| cache.between(at(a), at(b)).map(|e| e.id).collect::<Vec<_>>();
        assert_eq!(ids("2026-10-26T00:00:00Z", "2026-10-27T00:00:00Z"), [1]);
        assert_eq!(ids("2026-10-28T00:00:00Z", "2026-10-29T00:00:00Z"), [2]);
    }
}
