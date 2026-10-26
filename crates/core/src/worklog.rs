use serde::{Deserialize, Serialize};

use crate::{ActivityId, IssueKey, TimeRange};

/// Identifies a worklog in this app, whether or not Jira has seen it yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorklogId(pub u64);

/// Jira's id for a worklog.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RemoteWorklogId(pub String);

/// Time logged, or about to be logged, on an issue. This is the local, desired state;
/// [`crate::sync::plan`] compares it with the last copy pulled from Jira.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Worklog {
    pub id: WorklogId,
    /// Set once the worklog exists in Jira.
    pub remote_id: Option<RemoteWorklogId>,
    pub issue: IssueKey,
    pub range: TimeRange,
    pub comment: String,
    /// The activity this was created from, if any.
    pub origin: Option<ActivityId>,
}

/// A worklog as Jira last reported it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemoteWorklog {
    pub id: RemoteWorklogId,
    pub issue: IssueKey,
    pub range: TimeRange,
    pub comment: String,
}

impl Worklog {
    /// Whether this differs from what Jira has in a way a sync would have to write.
    pub fn differs_from(&self, remote: &RemoteWorklog) -> bool {
        self.issue != remote.issue || self.range != remote.range || self.comment != remote.comment
    }
}
