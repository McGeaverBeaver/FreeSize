//! Updates: finding a newer release on GitHub, checking it's signed with the
//! release key, and fetching the file for this platform. Nothing here
//! installs anything: the app and the command line decide what to do with
//! the verified bytes (`install` unpacks and swaps an executable on
//! desktops; Android hands the APK to the package installer, which asks).
//!
//! Checking is opt-in on each device, or the Hub checks once a day for all
//! of them and says in its poll answers when a newer release exists.

use freesize_proto::release::{matches_sums, release_key, verify_sums};
use serde::Deserialize;

use crate::transport::Transport;

/// The GitHub repository releases come from.
pub const REPOSITORY: &str = "McGeaverBeaver/freesize";

/// The releases page, for people.
pub const RELEASES_URL: &str = "https://github.com/McGeaverBeaver/freesize/releases";

/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Seconds a check or download may take.
const CHECK_TIMEOUT: u64 = 20;
const DOWNLOAD_TIMEOUT: u64 = 15 * 60;

/// Which releases a device follows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Channel {
    /// Releases marked as such on GitHub.
    #[default]
    Stable,
    /// Pre-releases too, for the people who help test.
    Beta,
}

impl Channel {
    pub fn id(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Beta => "beta",
        }
    }

    pub fn from_id(id: &str) -> Channel {
        if id.eq_ignore_ascii_case("beta") {
            Channel::Beta
        } else {
            Channel::Stable
        }
    }
}

/// A release on GitHub.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Release {
    /// "0.9.1".
    pub version: String,
    pub tag: String,
    /// Its page, with the notes.
    pub url: String,
    pub prerelease: bool,
    pub assets: Vec<Asset>,
}

/// One file of a release.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// Whether the release carries signed sums at all.
    pub fn signed(&self) -> bool {
        self.asset(SUMS).is_some() && self.asset(SIGNATURE).is_some()
    }
}

const SUMS: &str = "SHA256SUMS.txt";
const SIGNATURE: &str = "SHA256SUMS.txt.sig";

/// The numbers in a version like `0.9.1`, `v0.9.1` or "FreeSize app 0.9.1".
pub fn version_number(text: &str) -> Option<(u64, u64, u64)> {
    let word = text
        .split_whitespace()
        .last()?
        .trim_start_matches(|c: char| !c.is_ascii_digit());
    let mut parts = word.split('.').map(|p| {
        p.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u64>()
            .ok()
    });
    Some((
        parts.next()??,
        parts.next()??,
        parts.next().flatten().unwrap_or(0),
    ))
}

/// Whether `candidate` is a newer version than `current`.
pub fn newer(candidate: &str, current: &str) -> bool {
    match (version_number(candidate), version_number(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The file in a release for this build: the desktop archive or the APK.
pub fn asset_name() -> Option<&'static str> {
    if cfg!(target_os = "android") {
        Some("freesize-android.apk")
    } else if cfg!(all(windows, target_arch = "x86_64")) {
        Some("freesize-windows-x86_64.zip")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("freesize-linux-x86_64.tar.gz")
    } else {
        None
    }
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
}

fn headers() -> Vec<(&'static str, String)> {
    vec![
        ("User-Agent", format!("FreeSize/{CURRENT}")),
        ("Accept", "application/vnd.github+json".into()),
    ]
}

/// The newest release on `channel` from GitHub's answer, if it's newer
/// than `current`.
pub fn newest_in(json: &[u8], channel: Channel, current: &str) -> Result<Option<Release>, String> {
    let releases: Vec<ApiRelease> =
        serde_json::from_slice(json).map_err(|e| format!("GitHub's answer: {e}"))?;
    let mut best: Option<Release> = None;
    for release in releases {
        if release.draft || (release.prerelease && channel != Channel::Beta) {
            continue;
        }
        let Some(_) = version_number(&release.tag_name) else {
            continue;
        };
        let version = release.tag_name.trim_start_matches('v').to_string();
        if !newer(&version, current) || best.as_ref().is_some_and(|b| !newer(&version, &b.version))
        {
            continue;
        }
        best = Some(Release {
            version,
            tag: release.tag_name,
            url: release.html_url,
            prerelease: release.prerelease,
            assets: release
                .assets
                .into_iter()
                .map(|a| Asset {
                    name: a.name,
                    url: a.browser_download_url,
                    size: a.size,
                })
                .collect(),
        });
    }
    Ok(best)
}

/// Asks GitHub for the releases: the newest on `channel` that's newer than
/// `current`, or none.
pub fn check(
    transport: &dyn Transport,
    channel: Channel,
    current: &str,
) -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases?per_page=20");
    let (status, body) = transport.download(&url, &headers(), CHECK_TIMEOUT)?;
    if status != 200 {
        return Err(format!("GitHub answered {status}"));
    }
    newest_in(&body, channel, current)
}

fn fetch(transport: &dyn Transport, asset: &Asset) -> Result<Vec<u8>, String> {
    let (status, body) = transport.download(&asset.url, &headers(), DOWNLOAD_TIMEOUT)?;
    if status != 200 {
        return Err(format!("{}: GitHub answered {status}", asset.name));
    }
    Ok(body)
}

/// The release's sums file, checked against the release key this build
/// carries. A release without a signature, or one that doesn't verify, is
/// refused: nothing of it is installed.
pub fn verified_sums(transport: &dyn Transport, release: &Release) -> Result<String, String> {
    let key = release_key().ok_or(
        "this build carries no release key, so it can't check updates; install them by hand",
    )?;
    let (Some(sums), Some(signature)) = (release.asset(SUMS), release.asset(SIGNATURE)) else {
        return Err(format!(
            "release {} isn't signed; install it by hand if you trust it",
            release.version
        ));
    };
    let sums = fetch(transport, sums)?;
    let signature = String::from_utf8_lossy(&fetch(transport, signature)?).into_owned();
    if !verify_sums(&key, &sums, &signature) {
        return Err(format!(
            "release {}'s sums file is NOT signed by the release key {}: not installing it",
            release.version,
            key.fingerprint()
        ));
    }
    Ok(String::from_utf8_lossy(&sums).into_owned())
}

/// Downloads `name` from the release and checks it against the verified
/// sums; the bytes are the file as it was released, or nothing.
pub fn download(
    transport: &dyn Transport,
    release: &Release,
    sums: &str,
    name: &str,
) -> Result<Vec<u8>, String> {
    let asset = release
        .asset(name)
        .ok_or_else(|| format!("release {} has no {name}", release.version))?;
    let bytes = fetch(transport, asset)?;
    if !matches_sums(sums, name, &bytes) {
        return Err(format!(
            "{name} doesn't match the signed sums of release {}: not installing it",
            release.version
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_and_compared() {
        assert_eq!(version_number("0.9.1"), Some((0, 9, 1)));
        assert_eq!(version_number("v0.10.0"), Some((0, 10, 0)));
        assert_eq!(version_number("FreeSize app 0.8.8"), Some((0, 8, 8)));
        assert_eq!(version_number("1.2"), Some((1, 2, 0)));
        assert_eq!(version_number("latest"), None);
        assert!(newer("0.10.0", "0.9.1"));
        assert!(!newer("0.9.1", "0.9.1"));
        assert!(!newer("0.9.0", "0.9.1"));
        assert!(!newer("latest", "0.9.1"));
    }

    #[test]
    fn the_newest_release_on_the_channel_is_picked() {
        let json = br#"[
            {"tag_name": "v0.9.2-beta.1", "html_url": "u1", "prerelease": true, "assets": []},
            {"tag_name": "v0.9.1", "html_url": "u2", "assets": [
                {"name": "SHA256SUMS.txt", "browser_download_url": "s", "size": 100},
                {"name": "SHA256SUMS.txt.sig", "browser_download_url": "g", "size": 90}]},
            {"tag_name": "v0.9.3", "html_url": "u3", "draft": true, "assets": []},
            {"tag_name": "v0.8.8", "html_url": "u4", "assets": []}
        ]"#;
        let stable = newest_in(json, Channel::Stable, "0.9.0").unwrap().unwrap();
        assert_eq!(stable.version, "0.9.1");
        assert!(stable.signed());
        let beta = newest_in(json, Channel::Beta, "0.9.0").unwrap().unwrap();
        assert_eq!(beta.version, "0.9.2-beta.1");
        assert!(beta.prerelease && !beta.signed());
        assert_eq!(newest_in(json, Channel::Stable, "0.9.1").unwrap(), None);
        assert!(newest_in(b"not json", Channel::Stable, "0.9.1").is_err());
    }
}
