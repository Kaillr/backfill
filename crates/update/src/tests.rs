use backfill_http::fake::Fake;

use super::*;

fn release(tag: &str, prerelease: bool, draft: bool, asset: &str) -> String {
    format!(
        r#"{{"tag_name": "{tag}", "prerelease": {prerelease}, "draft": {draft},
            "assets": [{{"name": "{asset}", "browser_download_url": "https://example.com/{tag}/{asset}"}}]}}"#
    )
}

fn feed(releases: &[String]) -> Fake {
    let fake = Fake::new();
    fake.respond(Method::Get, "/releases", 200, &format!("[{}]", releases.join(",")));
    fake
}

#[test]
fn reads_versions_and_tags() {
    assert_eq!(Version::parse("0.2.0"), Some(Version(0, 2, 0)));
    assert_eq!(Version::parse("v1.10.3"), Some(Version(1, 10, 3)));
    assert_eq!(Version::parse("v1.0"), None);
    assert_eq!(Version::parse("1.0.0-beta.1"), None);
    assert_eq!(Version::parse("1.0.0.1"), None);
    assert!(Version(0, 10, 0) > Version(0, 9, 9));
    assert_eq!(Version(0, 1, 0).to_string(), "0.1.0");
}

#[test]
fn finds_the_newest_release_after_this_one() {
    let fake = feed(&[
        release("v0.3.0", true, true, ASSET),
        release("v0.2.1", true, false, "notes.txt"),
        release("v0.2.0", true, false, ASSET),
        release("v0.1.1", true, false, ASSET),
        release("v0.1.0", true, false, ASSET),
    ]);
    let found = newer(&fake, FEED, Version(0, 1, 0)).unwrap();
    // The draft and the release without the app don't count.
    assert_eq!(
        found,
        Some(Release { version: Version(0, 2, 0), download: format!("https://example.com/v0.2.0/{ASSET}") })
    );
    let request = &fake.requests.lock().unwrap()[0];
    assert!(request.headers.iter().any(|(name, value)| name == "User-Agent" && value == "backfill/0.1.0"));
}

#[test]
fn says_nothing_when_this_is_the_newest() {
    let fake = feed(&[release("v0.1.0", true, false, ASSET), release("v0.0.9", true, false, ASSET)]);
    assert_eq!(newer(&fake, FEED, Version(0, 1, 0)).unwrap(), None);
}

#[test]
fn stable_versions_skip_pre_releases() {
    let releases = [release("v1.1.0", true, false, ASSET), release("v1.0.1", false, false, ASSET)];
    assert_eq!(newer(&feed(&releases), FEED, Version(1, 0, 0)).unwrap().map(|r| r.version), Some(Version(1, 0, 1)));
    // A beta takes the stable release too.
    let releases = [release("v1.0.0", false, false, ASSET), release("v0.9.0", true, false, ASSET)];
    assert_eq!(newer(&feed(&releases), FEED, Version(0, 9, 0)).unwrap().map(|r| r.version), Some(Version(1, 0, 0)));
}

#[test]
fn says_why_github_had_no_answer() {
    let fake = Fake::new();
    fake.respond(Method::Get, "/releases", 403, "{}");
    assert_eq!(newer(&fake, FEED, Version(0, 1, 0)), Err("GitHub asked to wait before checking again".into()));
    let fake = Fake::new();
    fake.respond(Method::Get, "/releases", 200, "<html>");
    assert!(newer(&fake, FEED, Version(0, 1, 0)).unwrap_err().starts_with("GitHub's list of releases didn't read"));
}

#[test]
fn reads_the_requirement_of_a_signed_app() {
    let signed = "designated => identifier \"dev.backfill.app\" and certificate leaf = H\"52fd\"\n";
    assert_eq!(parse_requirement(signed).unwrap(), "identifier \"dev.backfill.app\" and certificate leaf = H\"52fd\"");
    // Ad hoc signatures and unsigned apps can't take updates.
    assert!(parse_requirement("# designated => cdhash H\"df80\"\n").is_err());
    assert!(parse_requirement("").is_err());
}

/// A pretend app bundle holding `text`.
fn bundle(path: &Path, text: &str) {
    std::fs::create_dir_all(path.join("Contents")).unwrap();
    std::fs::write(path.join("Contents/version"), text).unwrap();
}

fn contents(path: &Path) -> String {
    std::fs::read_to_string(path.join("Contents/version")).unwrap()
}

#[cfg(unix)]
#[test]
fn installing_swaps_the_app_once_it_has_quit() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Applications/backfill.app");
    let staged = dir.path().join("updates/0.2.0/backfill.app");
    bundle(&app, "0.1.0");
    bundle(&staged, "0.2.0");
    // A process that has already quit stands in for the app.
    let mut quit = Command::new("true").spawn().unwrap();
    quit.wait().unwrap();
    assert!(install_command(quit.id(), &staged, &app, false).status().unwrap().success());
    assert_eq!(contents(&app), "0.2.0");
    assert!(!staged.exists());
    assert!(!dir.path().join("updates/0.2.0/previous.app").exists());
}

#[cfg(unix)]
#[test]
fn a_failed_install_keeps_the_old_app() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Applications/backfill.app");
    let staged = dir.path().join("updates/0.2.0/backfill.app");
    bundle(&app, "0.1.0");
    std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
    let mut quit = Command::new("true").spawn().unwrap();
    quit.wait().unwrap();
    install_command(quit.id(), &staged, &app, false).status().unwrap();
    assert_eq!(contents(&app), "0.1.0");
}
