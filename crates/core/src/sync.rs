//! What a sync would write to Jira, worked out before anything is written.

use std::collections::HashMap;

use jiff::SignedDuration;

use crate::{RemoteWorklog, RemoteWorklogId, Worklog};

#[derive(Clone, Debug, PartialEq)]
pub enum SyncOp {
    Create(Worklog),
    /// Same issue, new time or comment.
    Update { before: RemoteWorklog, after: Worklog },
    /// Jira can't move a worklog to another issue, so this creates the new worklog first
    /// and then deletes the old one. If the create fails, nothing is lost.
    Move { before: RemoteWorklog, after: Worklog },
    Delete(RemoteWorklog),
}

impl SyncOp {
    /// The change in logged time this operation causes.
    pub fn delta(&self) -> SignedDuration {
        match self {
            SyncOp::Create(after) => after.range.duration(),
            SyncOp::Update { before, after } | SyncOp::Move { before, after } => {
                after.range.duration() - before.range.duration()
            }
            SyncOp::Delete(before) => -before.range.duration(),
        }
    }

    fn sort_key(&self) -> jiff::Timestamp {
        match self {
            SyncOp::Create(after) => after.range.start,
            SyncOp::Update { before, after } | SyncOp::Move { before, after } => {
                before.range.start.min(after.range.start)
            }
            SyncOp::Delete(before) => before.range.start,
        }
    }
}

/// Compares the local worklogs with the last copy pulled from Jira and lists what has to
/// change there, earliest first.
///
/// A remote worklog with no local counterpart was deleted here. A local worklog that
/// points at a remote id we never pulled is treated as new, which is the safe reading:
/// it creates rather than overwrites.
pub fn plan(remote: &[RemoteWorklog], local: &[Worklog]) -> Vec<SyncOp> {
    let by_id: HashMap<&RemoteWorklogId, &RemoteWorklog> =
        remote.iter().map(|r| (&r.id, r)).collect();
    let mut kept: Vec<&RemoteWorklogId> = Vec::new();
    let mut ops = Vec::new();

    for worklog in local {
        match worklog.remote_id.as_ref().and_then(|id| by_id.get(id)) {
            None => ops.push(SyncOp::Create(worklog.clone())),
            Some(&before) => {
                kept.push(&before.id);
                if before.issue != worklog.issue {
                    ops.push(SyncOp::Move {
                        before: before.clone(),
                        after: worklog.clone(),
                    });
                } else if worklog.differs_from(before) {
                    ops.push(SyncOp::Update {
                        before: before.clone(),
                        after: worklog.clone(),
                    });
                }
            }
        }
    }

    for before in remote {
        if !kept.contains(&&before.id) {
            ops.push(SyncOp::Delete(before.clone()));
        }
    }

    ops.sort_by_key(SyncOp::sort_key);
    ops
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueKey, TimeRange, WorklogId, time::at};
    use jiff::{civil::date, tz::TimeZone};

    fn range(from: i8, to: i8) -> TimeRange {
        let d = date(2026, 10, 26);
        TimeRange::new(at(d, from, 0, &TimeZone::UTC), at(d, to, 0, &TimeZone::UTC))
    }

    fn remote(id: &str, issue: &str, range: TimeRange) -> RemoteWorklog {
        RemoteWorklog {
            id: RemoteWorklogId(id.to_owned()),
            issue: IssueKey::parse(issue).unwrap(),
            range,
            comment: String::new(),
        }
    }

    fn local(id: u64, remote_id: Option<&str>, issue: &str, range: TimeRange) -> Worklog {
        Worklog {
            id: WorklogId(id),
            remote_id: remote_id.map(|r| RemoteWorklogId(r.to_owned())),
            issue: IssueKey::parse(issue).unwrap(),
            range,
            comment: String::new(),
            origin: None,
        }
    }

    #[test]
    fn nothing_changed_means_nothing_to_do() {
        let remote = [remote("1", "AB-1", range(8, 12))];
        let local = [local(1, Some("1"), "AB-1", range(8, 12))];
        assert!(plan(&remote, &local).is_empty());
    }

    #[test]
    fn lists_creates_updates_moves_and_deletes_in_time_order() {
        let remote = [
            remote("1", "AB-1", range(8, 10)),
            remote("2", "AB-1", range(10, 12)),
            remote("3", "AB-2", range(13, 14)),
        ];
        let local = [
            local(1, Some("1"), "AB-1", range(8, 11)),
            local(2, Some("2"), "AB-3", range(10, 12)),
            local(4, None, "AB-2", range(14, 16)),
        ];
        let ops = plan(&remote, &local);
        assert!(matches!(&ops[0], SyncOp::Update { after, .. } if after.id == WorklogId(1)));
        assert!(matches!(&ops[1], SyncOp::Move { after, .. } if after.issue.as_str() == "AB-3"));
        assert!(matches!(&ops[2], SyncOp::Delete(before) if before.id.0 == "3"));
        assert!(matches!(&ops[3], SyncOp::Create(after) if after.id == WorklogId(4)));
        assert_eq!(ops.len(), 4);
    }

    #[test]
    fn a_remote_id_we_never_pulled_creates_instead_of_overwriting() {
        let local = [local(1, Some("99"), "AB-1", range(8, 9))];
        assert!(matches!(plan(&[], &local)[..], [SyncOp::Create(_)]));
    }

    #[test]
    fn deltas_add_up_to_the_change_in_hours() {
        let remote = [remote("1", "AB-1", range(8, 10)), remote("2", "AB-1", range(13, 14))];
        let local = [local(1, Some("1"), "AB-1", range(8, 11)), local(3, None, "AB-2", range(15, 17))];
        let total: SignedDuration = plan(&remote, &local).iter().map(SyncOp::delta).sum();
        assert_eq!(total, SignedDuration::from_hours(2));
    }
}
