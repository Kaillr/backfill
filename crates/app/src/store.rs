//! What backfill keeps between runs. Settings and caches are JSON files in Application
//! Support; API tokens are in the Keychain and never in a file.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use backfill_git::Repo;

use serde::{Deserialize, Serialize, de::DeserializeOwned};

const KEYCHAIN_SERVICE: &str = "backfill";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Such as `example.atlassian.net`.
    pub jira_site: String,
    pub jira_email: String,
    /// Jira projects time is logged on, such as `APP`. Empty means any.
    pub jira_projects: Vec<String>,
    /// The name of the Jira field with an issue's business unit. Empty skips business units.
    /// Set in this file only: few Jiras have such fields.
    pub business_unit_field: String,
    /// The name of the Jira field with more business units the issue serves.
    pub shared_units_field: String,
    /// Folders with git repositories, searched one level deep.
    pub code_folders: Vec<String>,
    /// Repositories in those folders that aren't read, by path. All the others are.
    pub ignored_repositories: Vec<String>,
    /// From before repositories were picked one by one: only repositories from these GitHub
    /// organisations. Picking any repository in Settings replaces it with the list above.
    pub organisations: Vec<String>,
    pub hours_per_day: f64,
    /// When the workday usually starts, as `HH:MM`.
    pub day_start: String,
    /// Look for a newer release when backfill opens, and download it.
    pub check_for_updates: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            jira_site: String::new(),
            jira_email: String::new(),
            jira_projects: Vec::new(),
            business_unit_field: String::new(),
            shared_units_field: String::new(),
            code_folders: default_code_folder().into_iter().collect(),
            ignored_repositories: Vec::new(),
            organisations: Vec::new(),
            hours_per_day: 7.5,
            day_start: "08:00".into(),
            check_for_updates: true,
        }
    }
}

/// The first of the usual places for repositories that this Mac has.
fn default_code_folder() -> Option<String> {
    let home = dirs::home_dir()?;
    ["git", "code", "src", "Developer", "Projects", "repos"]
        .into_iter()
        .find(|name| home.join(name).is_dir())
        .map(|name| format!("~/{name}"))
}

impl Settings {
    /// Whether git activity is read from `repo`.
    pub fn tracks(&self, repo: &Repo) -> bool {
        let ignored = self.ignored_repositories.iter().any(|p| Path::new(p) == repo.path);
        let in_organisation = self.organisations.is_empty()
            || repo.origin.as_deref().is_some_and(|o| backfill_git::in_organisation(o, &self.organisations));
        !ignored && in_organisation
    }

    /// Whether hours are credited to business units.
    pub fn business_units(&self) -> bool {
        !self.business_unit_field.trim().is_empty() || !self.shared_units_field.trim().is_empty()
    }

    pub fn jira_ready(&self) -> bool {
        !self.jira_site.trim().is_empty() && !self.jira_email.trim().is_empty()
    }

    /// The code folders with `~` expanded.
    pub fn folders(&self) -> Vec<PathBuf> {
        self.code_folders
            .iter()
            .map(|f| f.trim())
            .filter(|f| !f.is_empty())
            .map(|f| match (f.strip_prefix("~/"), dirs::home_dir()) {
                (Some(rest), Some(home)) => home.join(rest),
                _ => PathBuf::from(f),
            })
            .collect()
    }

    /// The workday start as hour and minute, or 08:00 if it doesn't read.
    pub fn start(&self) -> (i8, i8) {
        Self::parse_start(&self.day_start).unwrap_or((8, 0))
    }

    /// Reads a time like `08:30`.
    pub fn parse_start(text: &str) -> Option<(i8, i8)> {
        text.trim()
            .split_once(':')
            .and_then(|(h, m)| Some((h.parse().ok()?, m.parse().ok()?)))
            .filter(|(h, m): &(i8, i8)| (0..24).contains(h) && (0..60).contains(m))
    }

    /// The project keys, upper case, with anything that can't be part of a key dropped.
    pub fn projects(&self) -> Vec<String> {
        self.jira_projects
            .iter()
            .map(|p| p.trim().trim_end_matches('-').to_uppercase())
            .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            .collect()
    }

    /// The workday as the workspace understands it.
    pub fn workday(&self) -> backfill_core::workspace::Settings {
        backfill_core::workspace::Settings {
            day_start: self.start(),
            target: jiff::SignedDuration::from_mins((self.hours_per_day * 60.).round() as i64),
            projects: self.projects(),
            ..backfill_core::workspace::Settings::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Secret {
    JiraToken,
    TogglToken,
}

impl Secret {
    fn account(self) -> &'static str {
        match self {
            Secret::JiraToken => "jira-api-token",
            Secret::TogglToken => "toggl-api-token",
        }
    }
}

/// Where files live, and access to the Keychain.
#[derive(Clone)]
pub struct Store {
    dir: PathBuf,
    /// Tokens for this run only, instead of the Keychain.
    memory: Option<Arc<Mutex<HashMap<&'static str, String>>>>,
}

impl Store {
    /// Application Support, or `BACKFILL_DATA_DIR` when set. A separate data folder is
    /// for trying things out, so it never sees the tokens in the Keychain: tokens entered
    /// there last until backfill quits.
    pub fn open() -> Self {
        let separate = std::env::var_os("BACKFILL_DATA_DIR").map(PathBuf::from);
        let memory = separate.is_some().then(Arc::default);
        let dir = separate
            .or_else(|| dirs::data_dir().map(|d| d.join("backfill")))
            .unwrap_or_else(|| PathBuf::from(".backfill"));
        let _ = std::fs::create_dir_all(&dir);
        Self { dir, memory }
    }

    /// A store in a fresh temporary folder, with tokens in memory.
    #[cfg(test)]
    pub fn for_tests() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("backfill-store-{}-{n}", std::process::id()));
        Self { dir, memory: Some(Arc::default()) }
    }

    pub fn load<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let text = std::fs::read_to_string(self.dir.join(name)).ok()?;
        match serde_json::from_str(&text) {
            Ok(value) => Some(value),
            Err(error) => {
                eprintln!("store: ignoring unreadable {name}: {error}");
                None
            }
        }
    }

    /// Writes to a temporary file and renames it over the old one, so a crash mid-write
    /// never leaves a half-written file.
    pub fn save<T: Serialize>(&self, name: &str, value: &T) {
        let Ok(text) = serde_json::to_string(value) else { return };
        let path = self.dir.join(name);
        let temporary = self.dir.join(format!(".{name}.tmp"));
        if std::fs::write(&temporary, text).and_then(|_| std::fs::rename(&temporary, &path)).is_err() {
            eprintln!("store: couldn't write {name}");
        }
    }

    pub fn secret(&self, secret: Secret) -> Option<String> {
        if let Some(memory) = &self.memory {
            return memory.lock().ok()?.get(secret.account()).cloned();
        }
        keyring::Entry::new(KEYCHAIN_SERVICE, secret.account())
            .and_then(|entry| entry.get_password())
            .ok()
            .filter(|s| !s.trim().is_empty())
    }

    pub fn set_secret(&self, secret: Secret, value: &str) -> Result<(), String> {
        if let Some(memory) = &self.memory {
            let mut memory = memory.lock().map_err(|e| e.to_string())?;
            match value.trim() {
                "" => memory.remove(secret.account()),
                value => memory.insert(secret.account(), value.to_owned()),
            };
            return Ok(());
        }
        let entry = keyring::Entry::new(KEYCHAIN_SERVICE, secret.account()).map_err(|e| e.to_string())?;
        if value.trim().is_empty() {
            let _ = entry.delete_credential();
            Ok(())
        } else {
            entry.set_password(value.trim()).map_err(|e| e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_workday_start() {
        let settings = |start: &str| Settings { day_start: start.into(), ..Settings::default() };
        assert_eq!(settings("08:30").start(), (8, 30));
        assert_eq!(settings(" 7:00 ").start(), (7, 0));
        assert_eq!(settings("25:00").start(), (8, 0));
        assert_eq!(settings("soon").start(), (8, 0));
    }

    #[test]
    fn reads_project_keys_however_they_are_typed() {
        let settings = Settings { jira_projects: vec!["app".into(), " SUP- ".into(), "not a key".into()], ..Settings::default() };
        assert_eq!(settings.projects(), ["APP", "SUP"]);
    }

    #[test]
    fn reads_every_repository_except_the_ignored_ones() {
        let repo = |path: &str, owner: &str| Repo { path: PathBuf::from(path), origin: Some(format!("https://github.com/{owner}/x")) };
        let mut settings = Settings { ignored_repositories: vec!["/code/b".into()], ..Settings::default() };
        assert!(settings.tracks(&repo("/code/a", "acme")));
        assert!(!settings.tracks(&repo("/code/b", "acme")));
        // Older settings that named organisations still leave the others out.
        settings.organisations = vec!["acme".into()];
        assert!(!settings.tracks(&repo("/code/c", "other")));
        assert!(settings.tracks(&repo("/code/a", "acme")));
    }

    #[test]
    fn files_round_trip_and_bad_ones_are_ignored() {
        let dir = std::env::temp_dir().join(format!("backfill-store-test-{}", std::process::id()));
        let store = Store { dir: dir.clone(), memory: Some(Arc::default()) };
        std::fs::create_dir_all(&dir).unwrap();
        let settings = Settings { jira_site: "example.atlassian.net".into(), ..Settings::default() };
        store.save("settings.json", &settings);
        assert_eq!(store.load::<Settings>("settings.json"), Some(settings));
        std::fs::write(dir.join("broken.json"), "{").unwrap();
        assert_eq!(store.load::<Settings>("broken.json"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
