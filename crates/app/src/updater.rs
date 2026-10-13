//! Updates in the app: an opt-in daily check of GitHub's releases, or a
//! hint from the Hub, then a download that's checked against the release
//! key's signature before anything is installed. Desktops swap the
//! executable and restart; Android saves the verified APK and hands it to
//! the package installer, which asks.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use freesize_agent::Transport;
use freesize_agent::update::{self, Channel, Release};
use freesize_proto::LatestRelease;
use serde::{Deserialize, Serialize};

use crate::HomeRow;
use crate::home::{heading, notice};

/// Action ids of the Updates section's buttons.
pub const TOGGLE: &str = "update-toggle";
pub const CHANNEL: &str = "update-channel";
pub const CHECK: &str = "update-check";
pub const INSTALL: &str = "update-install";
pub const NOTES: &str = "update-notes";

/// How often the check runs while it's on.
const EVERY: i64 = 24 * 60 * 60;

type Shared<T> = Arc<Mutex<Option<T>>>;

/// The setting, kept in `update.json` in FreeSize's data folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// Off until the person turns it on.
    #[serde(default)]
    pub check: bool,
    #[serde(default)]
    pub channel: String,
    /// When the last check ran (seconds since 1970).
    #[serde(default)]
    pub checked: i64,
}

impl Settings {
    pub fn channel(&self) -> Channel {
        Channel::from_id(&self.channel)
    }
}

/// What a finished install did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    /// The program's files were swapped; start `exe` again.
    #[cfg_attr(target_os = "android", allow(dead_code))]
    Swapped(PathBuf),
    /// Android: the APK was saved there and handed to the installer, or
    /// couldn't be, and the text says what to do.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    Saved(String),
}

/// Where things stand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Stage {
    Idle,
    Checking,
    Found(Release),
    Downloading(Release),
    Failed(String),
}

pub struct Updater {
    pub settings: Settings,
    file: Option<PathBuf>,
    transport: Arc<dyn Transport + Send + Sync>,
    pub stage: Stage,
    /// What the Hub last said, with the device's own check on or off.
    pub from_hub: Option<LatestRelease>,
    checking: Option<Shared<Result<Option<Release>, String>>>,
    installing: Option<Shared<Result<Installed, String>>>,
}

impl Updater {
    pub fn new(data_dir: Option<PathBuf>, transport: Arc<dyn Transport + Send + Sync>) -> Updater {
        let file = data_dir.map(|d| d.join("update.json"));
        let settings = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        Updater {
            settings,
            file,
            transport,
            stage: Stage::Idle,
            from_hub: None,
            checking: None,
            installing: None,
        }
    }

    fn save(&self) {
        if let Some(file) = &self.file
            && let Ok(text) = serde_json::to_string_pretty(&self.settings)
        {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, text);
        }
    }

    /// Whether a check should run now: the setting is on and a day has
    /// passed since the last.
    pub fn due(&self, now: i64) -> bool {
        self.settings.check && now - self.settings.checked >= EVERY && self.checking.is_none()
    }

    /// Whether a job is running, so the controller keeps polling.
    pub fn busy(&self) -> bool {
        self.checking.is_some() || self.installing.is_some()
    }

    /// Turns the check on or off; on starts one.
    pub fn toggle(&mut self, now: i64) {
        self.settings.check = !self.settings.check;
        self.save();
        if self.settings.check {
            self.check(now);
        } else if matches!(self.stage, Stage::Failed(_)) {
            self.stage = Stage::Idle;
        }
    }

    /// Switches between stable releases and pre-releases too, and checks.
    pub fn switch_channel(&mut self, now: i64) {
        self.settings.channel = match self.settings.channel() {
            Channel::Stable => Channel::Beta,
            Channel::Beta => Channel::Stable,
        }
        .id()
        .to_string();
        self.save();
        if self.settings.check {
            self.check(now);
        }
    }

    /// Asks GitHub on a worker thread.
    pub fn check(&mut self, now: i64) {
        if self.checking.is_some() || self.installing.is_some() {
            return;
        }
        self.settings.checked = now;
        self.save();
        let channel = self.settings.channel();
        let transport = self.transport.clone();
        let slot: Shared<Result<Option<Release>, String>> = Arc::default();
        let worker = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-update-check".into())
            .spawn(move || {
                let result = update::check(transport.as_ref(), channel, update::CURRENT);
                *worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            });
        match spawned {
            Ok(_) => {
                self.checking = Some(slot);
                self.stage = Stage::Checking;
            }
            Err(err) => self.stage = Stage::Failed(format!("couldn't start the check: {err}")),
        }
    }

    /// The release to install: the one found here, or the Hub's hint as a
    /// release to look up.
    fn to_install(&self) -> Option<Release> {
        match &self.stage {
            Stage::Found(release) => Some(release.clone()),
            _ => None,
        }
    }

    /// Downloads, verifies and installs the found release on a worker
    /// thread. With only the Hub's hint, looks the release up first.
    pub fn install(&mut self) {
        if self.installing.is_some() || self.checking.is_some() {
            return;
        }
        let known = self.to_install();
        let channel = self.settings.channel();
        let transport = self.transport.clone();
        let slot: Shared<Result<Installed, String>> = Arc::default();
        let worker = slot.clone();
        let spawned = std::thread::Builder::new()
            .name("freesize-update".into())
            .spawn(move || {
                let result = known
                    .map(Ok)
                    .unwrap_or_else(|| {
                        update::check(transport.as_ref(), channel, update::CURRENT)?
                            .ok_or_else(|| "no newer release is out".to_string())
                    })
                    .and_then(|release| install_release(transport.as_ref(), &release));
                *worker.lock().unwrap_or_else(|e| e.into_inner()) = Some(result);
            });
        match spawned {
            Ok(_) => {
                self.installing = Some(slot);
                if let Some(release) = self.to_install() {
                    self.stage = Stage::Downloading(release);
                }
            }
            Err(err) => self.stage = Stage::Failed(format!("couldn't start the update: {err}")),
        }
    }

    /// The Hub's poll answer named a newer release.
    pub fn hub_said(&mut self, latest: Option<LatestRelease>) -> bool {
        let changed = latest != self.from_hub;
        if let Some(latest) = latest {
            self.from_hub = Some(latest);
        }
        changed
    }

    /// Picks up finished jobs. Returns a status line for the person, and
    /// the swapped program to restart, if any.
    pub fn poll(&mut self) -> (Option<String>, Option<PathBuf>) {
        fn finished<T>(slot: &Option<Shared<T>>) -> Option<T> {
            slot.as_ref()
                .and_then(|s| s.lock().unwrap_or_else(|e| e.into_inner()).take())
        }
        if let Some(result) = finished(&self.checking) {
            self.checking = None;
            self.stage = match result {
                Ok(Some(release)) => Stage::Found(release),
                Ok(None) => Stage::Idle,
                Err(err) => Stage::Failed(format!("Couldn't check for updates: {err}")),
            };
            return (None, None);
        }
        if let Some(result) = finished(&self.installing) {
            self.installing = None;
            return match result {
                Ok(Installed::Swapped(exe)) => {
                    self.stage = Stage::Idle;
                    (
                        Some("Updated; FreeSize is starting again.".into()),
                        Some(exe),
                    )
                }
                Ok(Installed::Saved(text)) => {
                    self.stage = Stage::Idle;
                    (Some(text), None)
                }
                Err(err) => {
                    self.stage = Stage::Failed(format!("Couldn't update: {err}"));
                    (Some(format!("Couldn't update: {err}")), None)
                }
            };
        }
        (None, None)
    }

    /// The home screen's Updates section.
    pub fn rows(&self, now: i64) -> Vec<HomeRow> {
        let settings = &self.settings;
        let mut rows = vec![heading(
            "Updates",
            &format!("FreeSize {}", update::CURRENT),
            Some((
                if settings.check {
                    "Turn off"
                } else {
                    "Turn on"
                },
                TOGGLE,
            )),
        )];
        let install_label = if cfg!(target_os = "android") {
            "Install"
        } else {
            "Update"
        };
        let how = if cfg!(target_os = "android") {
            "FreeSize downloads it, checks the release key's signature, and hands it to \
             Android's installer, which asks."
        } else {
            "FreeSize downloads it, checks the release key's signature, swaps its own files \
             and starts again."
        };
        match &self.stage {
            Stage::Checking => rows.push(notice("Checking GitHub for a newer release…", "", None)),
            Stage::Found(release) => rows.push(notice(
                &format!(
                    "FreeSize {} is out{}",
                    release.version,
                    if release.prerelease {
                        " (pre-release)"
                    } else {
                        ""
                    }
                ),
                &format!(
                    "{how}{}",
                    if release.signed() {
                        ""
                    } else {
                        " This release carries no signature, so it has to be installed by hand."
                    }
                ),
                Some(if release.signed() {
                    (install_label, INSTALL)
                } else {
                    ("Release notes", NOTES)
                }),
            )),
            Stage::Downloading(release) => rows.push(notice(
                &format!("Downloading FreeSize {}…", release.version),
                "It's checked against the signed sums before anything is installed.",
                None,
            )),
            Stage::Failed(err) => rows.push(notice(
                "Updating didn't work",
                err,
                Some(("Try again", CHECK)),
            )),
            Stage::Idle => {
                if let Some(hub) = &self.from_hub {
                    rows.push(notice(
                        &format!("Your Hub says FreeSize {} is out", hub.version),
                        how,
                        Some((install_label, INSTALL)),
                    ));
                } else if settings.check {
                    rows.push(notice(
                        "Up to date",
                        &if settings.checked == 0 {
                            "FreeSize asks GitHub once a day.".to_string()
                        } else {
                            format!(
                                "FreeSize asks GitHub once a day; last {}.",
                                crate::insights::ago(
                                    now.max(0) as u64,
                                    settings.checked.max(0) as u64
                                )
                            )
                        },
                        Some(("Check now", CHECK)),
                    ));
                } else {
                    rows.push(notice(
                        "FreeSize doesn't check for updates",
                        "Turn it on and FreeSize asks GitHub once a day whether a newer release is \
                         out. Nothing is installed without your say-so. A FreeSize Hub with its \
                         own check on tells this device too.",
                        None,
                    ));
                }
            }
        }
        if settings.check {
            let beta = settings.channel() == Channel::Beta;
            rows.push(notice(
                if beta {
                    "Following pre-releases too"
                } else {
                    "Following stable releases"
                },
                "Pre-releases are for the people who help test; they're signed like any release.",
                Some((
                    if beta {
                        "Stable only"
                    } else {
                        "Pre-releases too"
                    },
                    CHANNEL,
                )),
            ));
        }
        rows
    }
}

/// Fetches a release's file for this platform, verifies it and installs it.
fn install_release(transport: &dyn Transport, release: &Release) -> Result<Installed, String> {
    let name = update::asset_name().ok_or("no release is built for this platform")?;
    let sums = update::verified_sums(transport, release)?;
    let bytes = update::download(transport, release, &sums, name)?;
    install_bytes(release, name, &bytes)
}

#[cfg(not(target_os = "android"))]
fn install_bytes(_release: &Release, name: &str, bytes: &[u8]) -> Result<Installed, String> {
    use freesize_agent::install;
    let files = install::unpack(
        bytes,
        name,
        &[
            "freesize-gui",
            "freesize-gui.exe",
            "freesize",
            "freesize.exe",
        ],
    )?;
    let exe = install::current_exe()?;
    install::install_beside(&exe, &files)?;
    Ok(Installed::Swapped(exe))
}

#[cfg(target_os = "android")]
fn install_bytes(release: &Release, _name: &str, bytes: &[u8]) -> Result<Installed, String> {
    let dir = crate::locations::default_path().join(crate::cleanup::SAVED_APKS);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = dir.join(format!("FreeSize {}.apk", release.version));
    std::fs::write(&file, bytes).map_err(|e| format!("{}: {e}", file.display()))?;
    match crate::android::install_apk(&file) {
        Ok(()) => Ok(Installed::Saved(format!(
            "FreeSize {} is verified and saved in {}; Android's installer is asking.",
            release.version,
            crate::cleanup::SAVED_APKS
        ))),
        Err(err) => Ok(Installed::Saved(format!(
            "FreeSize {} is verified and saved in {}. Android's installer couldn't be opened \
             ({err}): open the file from your Files app to install it.",
            release.version,
            crate::cleanup::SAVED_APKS
        ))),
    }
}

/// Once the new program runs, the old file is removed.
pub fn tidy() {
    #[cfg(not(target_os = "android"))]
    if let Ok(exe) = freesize_agent::install::current_exe() {
        freesize_agent::install::tidy(&exe);
    }
}

/// Starts the swapped program and asks this one to quit.
pub fn relaunch(exe: &std::path::Path) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        freesize_agent::install::relaunch(exe, &args)?;
        // Let the new window come up before this one goes.
        std::thread::sleep(Duration::from_millis(300));
        let _ = slint::quit_event_loop();
        Ok(())
    }
    #[cfg(target_os = "android")]
    {
        let _ = (exe, Duration::ZERO);
        Err("not on Android".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoNetwork;

    impl Transport for NoNetwork {
        fn request(
            &self,
            _method: &str,
            _url: &str,
            _headers: &[(&str, String)],
            _body: Option<&[u8]>,
            _timeout_secs: u64,
            _trust: &freesize_agent::Trust,
        ) -> Result<(u16, Vec<u8>), String> {
            Err("no network in tests".into())
        }
    }

    fn updater(dir: &std::path::Path) -> Updater {
        Updater::new(Some(dir.to_path_buf()), Arc::new(NoNetwork))
    }

    #[test]
    fn the_setting_is_off_until_turned_on_and_is_kept() {
        let dir = std::env::temp_dir().join(format!("freesize-updater-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut u = updater(&dir);
        assert!(!u.settings.check);
        assert!(!u.due(1_000_000));
        let rows = u.rows(1_000_000);
        assert_eq!(rows[0].action.as_str(), "Turn on");
        assert!(rows[1].title.contains("doesn't check"));
        u.toggle(1_000_000);
        assert!(u.settings.check && u.settings.checked == 1_000_000);
        // Reloaded from disk.
        let again = updater(&dir);
        assert!(again.settings.check);
        assert!(!again.due(1_000_000 + 100));
        assert!(again.due(1_000_000 + EVERY));
        // The check without a network fails and says so.
        for _ in 0..200 {
            let _ = u.poll();
            if u.stage != Stage::Checking {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(u.stage, Stage::Failed(_)), "{:?}", u.stage);
        assert_eq!(u.rows(1_000_000)[1].action.as_str(), "Try again");
        u.switch_channel(1_000_001);
        assert_eq!(u.settings.channel(), Channel::Beta);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_hubs_hint_is_shown_without_the_devices_own_check() {
        let dir = std::env::temp_dir().join(format!("freesize-updater-hub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut u = updater(&dir);
        assert!(!u.hub_said(None));
        assert!(u.hub_said(Some(LatestRelease {
            version: "0.9.1".into(),
            url: "u".into(),
        })));
        let rows = u.rows(10);
        assert!(rows[1].title.contains("Your Hub says FreeSize 0.9.1"));
        assert_eq!(rows[1].key.as_str(), INSTALL);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
