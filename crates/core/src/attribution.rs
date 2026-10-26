//! How logged hours are credited to business units.
//!
//! Half of an issue's hours go to its business unit, and the other half is shared equally
//! by the units in its shared units field. Both fields are named in Settings. An issue with only one of the two gives that one all of the time.

use std::collections::HashMap;

use jiff::SignedDuration;

use crate::Issue;

pub const UNASSIGNED: &str = "No business unit";

/// Hours per business unit, largest first.
pub fn by_business_unit<'a>(
    logged: impl IntoIterator<Item = (&'a Issue, SignedDuration)>,
) -> Vec<(String, f64)> {
    let mut hours: HashMap<String, f64> = HashMap::new();
    for (issue, duration) in logged {
        let total = duration.as_secs() as f64 / 3600.0;
        let mut credit = |unit: &str, share: f64| *hours.entry(unit.to_owned()).or_default() += share;
        match (&issue.business_unit, issue.labels.is_empty()) {
            (Some(unit), true) => credit(unit, total),
            (Some(unit), false) => {
                credit(unit, total / 2.0);
                let each = total / 2.0 / issue.labels.len() as f64;
                for label in &issue.labels {
                    credit(label, each);
                }
            }
            (None, false) => {
                let each = total / issue.labels.len() as f64;
                for label in &issue.labels {
                    credit(label, each);
                }
            }
            (None, true) => credit(UNASSIGNED, total),
        }
    }
    let mut sorted: Vec<(String, f64)> = hours.into_iter().collect();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IssueKey, IssueKind, StatusCategory};

    fn issue(unit: Option<&str>, labels: &[&str]) -> Issue {
        Issue {
            key: IssueKey::parse("AB-1").unwrap(),
            summary: String::new(),
            kind: IssueKind::Task,
            status: StatusCategory::InProgress,
            epic: None,
            business_unit: unit.map(Into::into),
            labels: labels.iter().map(|l| (*l).into()).collect(),
            assigned_to_me: true,
        }
    }

    #[test]
    fn half_goes_to_the_unit_and_half_is_shared_by_the_labels() {
        let north = issue(Some("North"), &["East", "South", "North"]);
        let split = by_business_unit([(&north, SignedDuration::from_hours(60))]);
        assert_eq!(split, [("North".into(), 40.0), ("East".into(), 10.0), ("South".into(), 10.0)]);
    }

    #[test]
    fn a_lone_unit_or_lone_labels_take_everything() {
        let only_unit = issue(Some("Central"), &[]);
        let only_labels = issue(None, &["West", "South"]);
        let neither = issue(None, &[]);
        let split = by_business_unit([
            (&only_unit, SignedDuration::from_hours(2)),
            (&only_labels, SignedDuration::from_hours(2)),
            (&neither, SignedDuration::from_hours(1)),
        ]);
        assert_eq!(
            split,
            [("Central".into(), 2.0), (UNASSIGNED.into(), 1.0), ("South".into(), 1.0), ("West".into(), 1.0)]
        );
    }
}
