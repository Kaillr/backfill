//! What you did in git, read from the repositories on this computer.
//!
//! No GitHub account or token is involved: the repositories you work in are already on
//! disk. Only your own commits count, matched by the email git commits with, and an
//! optional list of organisations keeps it to work repositories. Git only knows when you
//! committed or switched branch, never for how long, so everything here is a point in
//! time.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use jiff::Timestamp;

#[derive(Clone, Debug, PartialEq)]
pub struct Repo {
    pub path: PathBuf,
    /// Where `origin` points, if anywhere.
    pub origin: Option<String>,
}

/// A moment in a repository: a commit, or switching to a branch.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub at: Timestamp,
    pub kind: EventKind,
    /// The commit subject or the branch name. Issue keys are found in this.
    pub text: String,
    pub repo: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventKind {
    Commit,
    Checkout,
}

/// Repositories in `folders` and one level below them. With `organisations`, only
/// repositories whose origin is in one of them, such as `acme`.
pub fn discover(folders: &[PathBuf], organisations: &[String]) -> Vec<Repo> {
    let mut found = Vec::new();
    for folder in folders {
        let mut candidates = vec![folder.clone()];
        if let Ok(entries) = std::fs::read_dir(folder) {
            let mut children: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
            children.sort();
            candidates.extend(children);
        }
        for path in candidates {
            if path.join(".git").exists() && !found.iter().any(|r: &Repo| r.path == path) {
                let origin = git(&path, &["config", "--get", "remote.origin.url"]).map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
                found.push(Repo { path, origin });
            }
        }
    }
    found.retain(|repo| organisations.is_empty() || repo.origin.as_deref().is_some_and(|o| in_organisation(o, organisations)));
    found
}

/// Whether a remote URL belongs to one of `organisations`, for both
/// `git@github.com:org/repo.git` and `https://github.com/org/repo`.
pub fn in_organisation(origin: &str, organisations: &[String]) -> bool {
    // Drop the scheme and the user, then the host: the path starts with the owner.
    let rest = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let rest = rest.rsplit_once('@').map_or(rest, |(_, rest)| rest);
    let path = rest.find([':', '/']).map_or("", |i| &rest[i + 1..]);
    let owner = path.split('/').next().unwrap_or("");
    organisations.iter().any(|org| org.trim().eq_ignore_ascii_case(owner))
}

/// The emails your commits are made with: the global one, plus any set in the repositories.
pub fn emails(repos: &[Repo]) -> Vec<String> {
    let mut emails: Vec<String> = Vec::new();
    let global = Command::new("git").args(["config", "--global", "--get", "user.email"]).output();
    let candidates = global
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .into_iter()
        .chain(repos.iter().filter_map(|r| git(&r.path, &["config", "--get", "user.email"])));
    for email in candidates {
        let email = email.trim().to_lowercase();
        if !email.is_empty() && !emails.contains(&email) {
            emails.push(email);
        }
    }
    emails
}

/// Your commits and branch switches in `repo` between `start` and `end`.
pub fn events(repo: &Repo, emails: &[String], start: Timestamp, end: Timestamp) -> Vec<Event> {
    let mut events = Vec::new();
    if !emails.is_empty() {
        let mut args = vec![
            "log".to_owned(),
            "--all".to_owned(),
            "--no-merges".to_owned(),
            format!("--since={}", start.as_second()),
            format!("--until={}", end.as_second()),
            "--format=%at%x09%ae%x09%s".to_owned(),
        ];
        args.extend(emails.iter().map(|e| format!("--author={e}")));
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        for line in git(&repo.path, &args).unwrap_or_default().lines() {
            let mut parts = line.splitn(3, '\t');
            let (Some(at), Some(author), Some(subject)) = (parts.next(), parts.next(), parts.next()) else { continue };
            // --author matches anywhere in name or email; keep exact matches only.
            if !emails.iter().any(|e| e.eq_ignore_ascii_case(author.trim())) {
                continue;
            }
            if let Some(at) = at.parse().ok().and_then(|s| Timestamp::from_second(s).ok()) {
                events.push(Event { at, kind: EventKind::Commit, text: subject.to_owned(), repo: repo.path.clone() });
            }
        }
    }
    // The reflog is this clone's own history, so every checkout in it is yours.
    for line in git(&repo.path, &["reflog", "--date=unix", "--format=%gd%x09%gs"]).unwrap_or_default().lines() {
        let Some((selector, subject)) = line.split_once('\t') else { continue };
        let Some(to) = subject.strip_prefix("checkout: moving from ").and_then(|s| s.split_once(" to ")).map(|(_, to)| to) else {
            continue;
        };
        let Some(at) = selector
            .rsplit_once('{')
            .and_then(|(_, rest)| rest.strip_suffix('}'))
            .and_then(|s| s.parse().ok())
            .and_then(|s| Timestamp::from_second(s).ok())
        else {
            continue;
        };
        if at >= start && at < end {
            events.push(Event { at, kind: EventKind::Checkout, text: to.to_owned(), repo: repo.path.clone() });
        }
    }
    events.sort_by_key(|e| e.at);
    events
}

fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(repo).args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str], date: &str) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&status.stderr));
    }

    /// A repository with two of my commits, one by someone else, and a branch switch.
    fn repo(origin: &str) -> (tempfile::TempDir, Repo) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("webapp");
        std::fs::create_dir(&path).unwrap();
        let date = "2026-10-26T08:00:00+00:00";
        run(&path, &["init", "-q", "-b", "main"], date);
        run(&path, &["config", "user.email", "me@example.com"], date);
        run(&path, &["config", "user.name", "Me"], date);
        run(&path, &["remote", "add", "origin", origin], date);
        run(&path, &["commit", "-q", "--allow-empty", "-m", "APP-123: Fix the export button"], "2026-10-26T09:30:00+00:00");
        run(&path, &["checkout", "-q", "-b", "APP-124-login-page"], "2026-10-26T10:00:00+00:00");
        run(&path, &["commit", "-q", "--allow-empty", "-m", "APP-124: Add the login page"], "2026-10-26T11:15:00+00:00");
        run(
            &path,
            &["-c", "user.email=colleague@example.com", "commit", "-q", "--allow-empty", "-m", "APP-9: Not mine"],
            "2026-10-26T12:00:00+00:00",
        );
        let repo = discover(&[dir.path().to_path_buf()], &[]).remove(0);
        (dir, repo)
    }

    fn at(text: &str) -> Timestamp {
        text.parse().unwrap()
    }

    #[test]
    fn finds_repositories_one_level_down_with_their_origin() {
        let (dir, repo) = repo("git@github.com:acme/webapp.git");
        assert_eq!(repo.path, dir.path().join("webapp"));
        assert_eq!(repo.origin.as_deref(), Some("git@github.com:acme/webapp.git"));
    }

    #[test]
    fn an_organisation_filter_keeps_only_its_repositories() {
        let (dir, _) = repo("https://github.com/acme/webapp");
        assert_eq!(discover(&[dir.path().to_path_buf()], &["Acme".into()]).len(), 1);
        assert!(discover(&[dir.path().to_path_buf()], &["someone-else".into()]).is_empty());
    }

    #[test]
    fn recognises_the_owner_in_both_kinds_of_remote() {
        let orgs = ["acme".to_owned()];
        assert!(in_organisation("git@github.com:acme/app.git", &orgs));
        assert!(in_organisation("https://github.com/acme/app", &orgs));
        assert!(in_organisation("ssh://git@github.com/acme/app.git", &orgs));
        assert!(!in_organisation("git@github.com:acmecorp/app.git", &orgs));
        assert!(!in_organisation("https://github.com/other/acme", &orgs));
    }

    #[test]
    fn reads_my_commits_and_branch_switches_as_moments() {
        let (_dir, repo) = repo("git@github.com:acme/webapp.git");
        let events = events(&repo, &["me@example.com".into()], at("2026-10-26T00:00:00Z"), at("2026-10-27T00:00:00Z"));
        let summary: Vec<(EventKind, &str)> = events.iter().map(|e| (e.kind, e.text.as_str())).collect();
        assert_eq!(
            summary,
            [
                (EventKind::Commit, "APP-123: Fix the export button"),
                (EventKind::Checkout, "APP-124-login-page"),
                (EventKind::Commit, "APP-124: Add the login page"),
            ]
        );
        assert_eq!(events[0].at, at("2026-10-26T09:30:00Z"));
    }

    #[test]
    fn nothing_outside_the_window() {
        let (_dir, repo) = repo("git@github.com:acme/webapp.git");
        assert!(events(&repo, &["me@example.com".into()], at("2026-10-27T00:00:00Z"), at("2026-10-28T00:00:00Z")).is_empty());
    }
}
