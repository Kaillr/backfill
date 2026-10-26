use std::fmt;

use serde::{Deserialize, Serialize};

/// A Jira issue key such as `APP-123`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IssueKey(String);

impl IssueKey {
    /// Accepts exactly one key: a project of an uppercase letter followed by uppercase
    /// letters or digits, a dash, and a number.
    pub fn parse(text: &str) -> Option<Self> {
        let (project, number) = text.split_once('-')?;
        let mut chars = project.chars();
        let valid_project = chars.next().is_some_and(|c| c.is_ascii_uppercase())
            && project.len() >= 2
            && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit());
        let valid_number = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());
        (valid_project && valid_number).then(|| Self(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn project(&self) -> &str {
        self.0.split_once('-').map_or(&self.0, |(p, _)| p)
    }
}

impl fmt::Display for IssueKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Every issue key mentioned in `text`, in order, without repeats.
///
/// Matches commit subjects (`APP-123: Fix …`), branch names (`APP-123-fix-thing`),
/// merge commits (`Merge pull request #1 from org/APP-123-fix`) and meeting titles.
pub fn find_issue_keys(text: &str) -> Vec<IssueKey> {
    let bytes = text.as_bytes();
    let mut keys: Vec<IssueKey> = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let starts_word = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        if !(starts_word && bytes[i].is_ascii_uppercase()) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < bytes.len() && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit()) {
            j += 1;
        }
        if j - i < 2 || j >= bytes.len() || bytes[j] != b'-' {
            i = j.max(i + 1);
            continue;
        }
        let digits_start = j + 1;
        let mut k = digits_start;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            k += 1;
        }
        let ends_word = k == bytes.len() || !bytes[k].is_ascii_alphabetic();
        if k > digits_start
            && ends_word
            && let Some(key) = IssueKey::parse(&text[i..k])
            && !keys.contains(&key)
        {
            keys.push(key);
        }
        i = k.max(i + 1);
    }
    keys
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IssueKind {
    Epic,
    Story,
    Task,
    Bug,
    SubTask,
    /// A catch-all issue for time that isn't tied to a ticket, suggested last.
    WorkLog,
    Other,
}

impl IssueKind {
    pub fn label(self) -> &'static str {
        match self {
            IssueKind::Epic => "Epic",
            IssueKind::Story => "Story",
            IssueKind::Task => "Task",
            IssueKind::Bug => "Bug",
            IssueKind::SubTask => "Sub-task",
            IssueKind::WorkLog => "Work log",
            IssueKind::Other => "Issue",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StatusCategory {
    ToDo,
    InProgress,
    Done,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    pub key: IssueKey,
    pub summary: String,
    pub kind: IssueKind,
    pub status: StatusCategory,
    pub epic: Option<IssueKey>,
    /// The business unit field named in Settings: half of the logged hours are credited to it.
    pub business_unit: Option<String>,
    /// The shared units field named in Settings: the other half is split across these.
    pub labels: Vec<String>,
    pub assigned_to_me: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(text: &str) -> Vec<String> {
        find_issue_keys(text).iter().map(|k| k.to_string()).collect()
    }

    #[test]
    fn parses_only_whole_keys() {
        assert!(IssueKey::parse("APP-123").is_some());
        assert!(IssueKey::parse("A1-2").is_some());
        assert!(IssueKey::parse("app-123").is_none());
        assert!(IssueKey::parse("C-1").is_none());
        assert!(IssueKey::parse("APP-").is_none());
        assert!(IssueKey::parse("APP-12a").is_none());
    }

    #[test]
    fn finds_keys_in_commit_subjects_and_branches() {
        assert_eq!(keys("APP-123: Fix the export button"), ["APP-123"]);
        assert_eq!(keys("APP-321-empty-state"), ["APP-321"]);
        assert_eq!(
            keys("Merge pull request #42 from org/APP-124-login-page"),
            ["APP-124"]
        );
    }

    /// Any key-shaped word counts here; whether the project exists is Jira's call.
    #[test]
    fn rejects_keys_glued_to_letters_and_repeats() {
        assert_eq!(keys("UTF-8 and ISO-8601abc"), ["UTF-8"]);
        assert_eq!(keys("xAPP-1 APP-2 APP-2"), ["APP-2"]);
        assert_eq!(keys("Workshop APP-7/APP-8"), ["APP-7", "APP-8"]);
    }

    #[test]
    fn knows_its_project() {
        assert_eq!(IssueKey::parse("APP-1").unwrap().project(), "APP");
    }
}
