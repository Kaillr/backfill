//! Keeps backfill up to date from its releases on GitHub.
//!
//! A newer release is downloaded and unpacked into a cache folder, then checked: it has to
//! be signed with the same certificate as the app that's running, and be the version the
//! release says. Only then is it offered. It replaces the app once backfill has quit, so
//! nothing changes under a running app, and is opened again when the update was a restart.
//!
//! The certificate is backfill's own, self-signed, so the check needs no Apple account and
//! keeps working offline. It also keeps the app's identity the same from one version to
//! the next, so macOS remembers its calendar access and Keychain items across updates.

use std::{
    fmt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use backfill_http::{Http, Method, Request, Transport};
use serde::Deserialize;

/// The releases, newest first.
pub const FEED: &str = "https://api.github.com/repos/Kaillr/backfill/releases?per_page=20";
/// The file each release carries the app in.
pub const ASSET: &str = "backfill-macos.zip";
const APP: &str = "backfill.app";

/// A plain `major.minor.patch` version. Releases never carry a pre-release suffix: while
/// backfill is 0.x, every release is a beta.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u64, pub u64, pub u64);

impl Version {
    /// Reads `0.2.0`, or a tag like `v0.2.0`.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.trim().trim_start_matches('v').split('.').map(|p| p.parse().ok());
        let version = Version(parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(version)
    }

    pub fn is_beta(self) -> bool {
        self.0 == 0
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    /// Where the zipped app is.
    pub download: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// The newest release after `current`, if there is one. Pre-releases count only while
/// `current` is a beta itself, so betas follow betas and 1.0 on follows stable releases.
pub fn newer(http: &dyn Transport, feed: &str, current: Version) -> Result<Option<Release>, String> {
    let request = Request::new(Method::Get, feed)
        .header("Accept", "application/vnd.github+json")
        .header("User-Agent", format!("backfill/{current}"));
    let response = http.send(request).map_err(|e| format!("Couldn't reach GitHub: {e}"))?;
    if !response.ok() {
        return Err(match response.status {
            403 | 429 => "GitHub asked to wait before checking again".into(),
            status => format!("GitHub answered {status}"),
        });
    }
    let releases: Vec<GithubRelease> =
        serde_json::from_str(&response.body).map_err(|e| format!("GitHub's list of releases didn't read: {e}"))?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft && (current.is_beta() || !r.prerelease))
        .filter_map(|r| {
            let version = Version::parse(&r.tag_name)?;
            let asset = r.assets.into_iter().find(|a| a.name == ASSET)?;
            Some(Release { version, download: asset.browser_download_url })
        })
        .filter(|r| r.version > current)
        .max_by_key(|r| r.version))
}

/// Downloads updates for the app at `app` into `folder`, and checks them.
pub struct Updater {
    /// The running app's bundle, which an update replaces.
    pub app: PathBuf,
    /// Where updates are unpacked, one folder per version.
    pub folder: PathBuf,
}

impl Updater {
    /// Whether this app can replace itself. It can't while macOS runs it from a read-only
    /// copy, which it does for apps opened straight from Downloads, or when its folder
    /// isn't writable.
    pub fn can_replace(&self) -> Result<(), String> {
        if self.app.components().any(|c| c.as_os_str() == "AppTranslocation") {
            return Err("Move backfill to your Applications folder to get updates".into());
        }
        let parent = self.app.parent().ok_or("backfill isn't in a folder")?;
        let probe = parent.join(".backfill-update-check");
        std::fs::create_dir(&probe)
            .and_then(|_| std::fs::remove_dir(&probe))
            .map_err(|_| format!("backfill can't replace itself in {}", parent.display()))
    }

    /// Downloads and unpacks `release`, unless that's been done already, and checks it.
    /// Returns the unpacked app, ready to install.
    pub fn prepare(&self, release: &Release, http: &Http) -> Result<PathBuf, String> {
        let dir = self.folder.join(release.version.to_string());
        let staged = dir.join(APP);
        if staged.exists() && self.check(&staged, release.version).is_ok() {
            return Ok(staged);
        }
        // Only the newest update is kept.
        self.clear();
        std::fs::create_dir_all(&dir).map_err(|e| format!("Couldn't make a folder for the update: {e}"))?;
        let zip = dir.join(ASSET);
        http.download(&release.download, &zip).map_err(|e| format!("Couldn't download the update: {e}"))?;
        run(Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(&zip).arg(&dir))
            .map_err(|e| format!("Couldn't unpack the update: {e}"))?;
        let _ = std::fs::remove_file(&zip);
        if let Err(error) = self.check(&staged, release.version) {
            self.clear();
            return Err(error);
        }
        Ok(staged)
    }

    /// Removes downloaded updates.
    pub fn clear(&self) {
        let _ = std::fs::remove_dir_all(&self.folder);
    }

    /// Whether `staged` is signed with the running app's certificate, under the same
    /// identifier, and is `version`. The version is read from inside the signed bundle, so
    /// an old release can't pass for a new one.
    fn check(&self, staged: &Path, version: Version) -> Result<(), String> {
        let requirement = designated_requirement(&self.app)?;
        run(Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "-R"])
            .arg(format!("={requirement}"))
            .arg(staged))
        .map_err(|_| "The update isn't signed like this copy of backfill, so it wasn't installed".to_owned())?;
        let found = run(Command::new("/usr/bin/plutil")
            .args(["-extract", "CFBundleShortVersionString", "raw", "-o", "-"])
            .arg(staged.join("Contents/Info.plist")))?;
        match Version::parse(&found) {
            Some(v) if v == version => Ok(()),
            _ => Err(format!("The update says it's {}, not {version}", found.trim())),
        }
    }
}

/// What the running app's signature requires of code that is the same app: its
/// identifier, and the certificate it was signed with.
fn designated_requirement(app: &Path) -> Result<String, String> {
    let output = run(Command::new("/usr/bin/codesign").args(["--display", "--requirements", "-"]).arg(app))?;
    parse_requirement(&output)
}

fn parse_requirement(output: &str) -> Result<String, String> {
    let requirement = output
        .lines()
        .find_map(|line| line.trim_start_matches("# ").strip_prefix("designated => "))
        .ok_or("This copy of backfill isn't signed")?;
    // An ad hoc signature names one exact build, which no update can match.
    if requirement.starts_with("cdhash") {
        return Err("This copy of backfill isn't signed for updates".into());
    }
    Ok(requirement.to_owned())
}

/// Runs `command` and returns what it printed, or what it complained about.
fn run(command: &mut Command) -> Result<String, String> {
    let output = command.stdin(Stdio::null()).output().map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Waits for the app to quit, moves the old app aside, moves the new one into its place
/// and removes the old one. If the new one can't be moved in, the old one goes back.
const INSTALL: &str = r#"
pid=$1 new=$2 app=$3 relaunch=$4
while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done
old="$(dirname "$new")/previous.app"
rm -rf "$old"
if mv "$app" "$old"; then
    if mv "$new" "$app"; then
        rm -rf "$old"
    else
        mv "$old" "$app"
    fi
fi
if [ "$relaunch" = 1 ]; then open "$app"; fi
"#;

/// Replaces `app` with `staged` once this process has quit, and opens it again if
/// `relaunch`. Call it just before quitting.
pub fn install_after_quit(staged: &Path, app: &Path, relaunch: bool) -> Result<(), String> {
    let mut command = install_command(std::process::id(), staged, app, relaunch);
    // Its own process group, so it outlives the app.
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    command.spawn().map(drop).map_err(|e| format!("Couldn't start the update: {e}"))
}

fn install_command(pid: u32, staged: &Path, app: &Path, relaunch: bool) -> Command {
    let mut command = Command::new("/bin/sh");
    command
        .arg("-c")
        .arg(INSTALL)
        .arg("backfill-update")
        .arg(pid.to_string())
        .arg(staged)
        .arg(app)
        .arg(if relaunch { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[cfg(test)]
mod tests;
