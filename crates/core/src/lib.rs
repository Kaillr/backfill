//! Domain model for backfill: what happened (activities), what is logged (worklogs),
//! and the rules that turn one into the other.
//!
//! Nothing in here knows about the UI or about any remote service.

pub mod activity;
pub mod attribution;
#[cfg(any(test, feature = "fixtures"))]
pub mod demo;
pub mod fill;
pub mod history;
pub mod issue;
pub mod layout;
pub mod suggest;
pub mod sync;
pub mod time;
pub mod worklog;
pub mod workspace;

pub use activity::{Activity, ActivityId, Source};
pub use history::JiraHistory;
pub use issue::{Issue, IssueKey, IssueKind, StatusCategory};
pub use time::TimeRange;
pub use worklog::{RemoteWorklog, RemoteWorklogId, Worklog, WorklogId};
