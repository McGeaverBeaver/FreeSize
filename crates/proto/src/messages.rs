//! The messages, all JSON. Fields added later get defaults, so older devices
//! and Hubs keep understanding each other.
//!
//! | Request | Body | Answer |
//! |---|---|---|
//! | `GET /api/v1/hello` | | [`Hello`] |
//! | `POST /api/v1/pair` | `Signed<PairRequest>` by the new device | `Signed<PairResponse>` by the Hub |
//! | `POST /api/v1/report` | [`Report`] | |
//! | `POST /api/v1/poll` | [`PollRequest`] | [`PollResponse`] |
//! | `POST /api/v1/commands/{id}` | [`CommandUpdate`] | |
//! | `POST /api/v1/decisions` | `Signed<Decision>` by the approver's device | |
//! | `GET /api/v1/devices/{fingerprint}` | | [`DeviceKey`] |
//!
//! Everything but `hello` and `pair` is a signed device request (see
//! [`crate::signing`]).

use freesize_core::snapshot::FolderSize;
use serde::{Deserialize, Serialize};

use crate::keys::PublicKey;
use crate::signing::Signed;

/// Version of this protocol.
pub const PROTOCOL: u32 = 1;

/// What the Hub says about itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    pub name: String,
    pub version: String,
    pub protocol: u32,
    pub key: PublicKey,
    /// The Hub's own certificate authority (PEM), when it serves HTTPS with
    /// a certificate it made itself. A device trusts it for this Hub's
    /// address only. Missing when the Hub has an administrator's certificate
    /// or a reverse proxy does TLS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
}

/// Where the Hub is reached from now on, signed by the Hub: sent to a device
/// still using an older address (plain `http://` from before the Hub served
/// HTTPS). The device checks the signature with the Hub key it paired with,
/// then that the Hub answers there with the same key, before switching.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubAddress {
    pub url: String,
    /// As in [`Hello::ca`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
    pub time: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Android,
    Windows,
    Macos,
    Linux,
    /// The FreeSize Nextcloud app (0.20). Older Hubs read it as Other.
    Nextcloud,
    #[default]
    #[serde(other)]
    Other,
}

impl Platform {
    /// The platform this is built for.
    pub fn current() -> Platform {
        if cfg!(target_os = "android") {
            Platform::Android
        } else if cfg!(windows) {
            Platform::Windows
        } else if cfg!(target_os = "macos") {
            Platform::Macos
        } else if cfg!(target_os = "linux") {
            Platform::Linux
        } else {
            Platform::Other
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Platform::Android => "Android",
            Platform::Windows => "Windows",
            Platform::Macos => "macOS",
            Platform::Linux => "Linux",
            Platform::Nextcloud => "Nextcloud",
            Platform::Other => "Other",
        }
    }
}

/// The kinds of action a device can be asked to do. A grant covers one kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Delete,
    Uninstall,
    Clean,
    /// Put quarantined files back.
    Restore,
    /// Delete quarantined batches for good. Never covered by a grant.
    Empty,
    /// Remove Docker images or build cache. Never covered by a grant.
    Docker,
    /// Free the local copies of synced files (make them online-only), or
    /// keep them on the device again (0.13). Comes back with a click.
    OnlineOnly,
    /// Move a folder into the Nextcloud client's sync folder (0.13).
    MoveIn,
    /// Add a share to Nextcloud as external storage, on Unraid (0.13).
    Mount,
    /// One a newer device or Hub knows and this one doesn't.
    #[serde(other)]
    Unknown,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            ActionKind::Delete => "delete files",
            ActionKind::Uninstall => "uninstall apps",
            ActionKind::Clean => "clean up junk",
            ActionKind::Restore => "put back files",
            ActionKind::Empty => "empty the quarantine",
            ActionKind::Docker => "remove Docker images",
            ActionKind::OnlineOnly => "free local copies of synced files",
            ActionKind::MoveIn => "move folders into the Nextcloud folder",
            ActionKind::Mount => "add shares to Nextcloud",
            ActionKind::Unknown => "do something this version doesn't know",
        }
    }

    /// Whether "approve for N minutes" may cover this kind. What can't be
    /// put back (emptying the quarantine, removing images) is asked for
    /// every time, with the paths shown, however long a grant for deleting
    /// is still running.
    pub fn grantable(self) -> bool {
        matches!(
            self,
            ActionKind::Delete
                | ActionKind::Uninstall
                | ActionKind::Clean
                | ActionKind::Restore
                | ActionKind::OnlineOnly
        )
    }
}

/// Permission an admin was given on a device to do one kind of action
/// without asking again, until it expires.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    /// The admin's stable id from the sign-in provider.
    pub admin: String,
    pub admin_name: String,
    pub kind: ActionKind,
    /// Seconds since 1970.
    pub expires: i64,
}

/// What a device says about itself, at pairing and whenever it changes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceInfo {
    pub name: String,
    pub platform: Platform,
    /// "Android 16", "Windows 11", "Ubuntu 24.04".
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub hostname: String,
    /// "FreeSize app 0.7.0", "FreeSize agent 0.7.0".
    #[serde(default)]
    pub app: String,
    /// The actions this device can carry out.
    #[serde(default)]
    pub actions: Vec<ActionKind>,
    /// Whether someone can approve on the device itself.
    #[serde(default)]
    pub attended: bool,
    /// For a device nobody sits at: the fingerprint of the designated
    /// approver's device, set on the device itself.
    #[serde(default)]
    pub approver: Option<String>,
    /// Grants active on the device.
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// Whose eyes its scans see with: "root", "administrator" or "user"
    /// (empty from devices before 0.8.8). A user's scans miss other users'
    /// folders and the system's own.
    #[serde(default)]
    pub rights: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairRequest {
    pub code: String,
    pub key: PublicKey,
    pub info: DeviceInfo,
    pub time: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairResponse {
    /// The id the device uses from now on.
    pub device: String,
    pub hub: Hello,
    /// The device's own key, so the device knows the answer is for it.
    pub device_key: PublicKey,
}

/// What a device sends: any of its parts that changed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Report {
    #[serde(default)]
    pub info: Option<DeviceInfo>,
    #[serde(default)]
    pub disks: Option<Vec<Disk>>,
    #[serde(default)]
    pub scan: Option<ScanReport>,
    #[serde(default)]
    pub apps: Option<AppsReport>,
    /// An Unraid server's array, shares, Docker and VMs.
    #[serde(default)]
    pub unraid: Option<crate::unraid::UnraidReport>,
    /// What the device's quarantine holds, to put back or empty.
    #[serde(default)]
    pub quarantine: Option<Vec<crate::unraid::QuarantineBatch>>,
    /// What the junk cleaner found (0.10).
    #[serde(default)]
    pub junk: Option<JunkReport>,
    /// What the device keeps of its Nextcloud folders (0.12).
    #[serde(default)]
    pub sync: Option<crate::sync::SyncReport>,
    /// The FreeSize Nextcloud app's look at its users' files (0.20).
    #[serde(default)]
    pub cloud: Option<crate::cloud::CloudReport>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub name: String,
    pub path: String,
    pub total: u64,
    pub free: u64,
}

/// A finished scan: totals, the biggest folders and files, and file types.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScanReport {
    /// The scanned folder as the device writes it.
    pub root: String,
    /// "Internal storage", "C:", "Videos".
    #[serde(default)]
    pub label: String,
    /// Seconds since 1970.
    pub taken: i64,
    pub size: u64,
    pub allocated: u64,
    pub files: u64,
    pub dirs: u64,
    #[serde(default)]
    pub errors: u64,
    #[serde(default = "yes")]
    pub complete: bool,
    /// The first folders the scan couldn't list, relative to `root` ("" for
    /// the root itself), so the dashboard can name what's missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unreadable: Vec<String>,
    /// Folder sizes, paths relative to `root` with `/` between names.
    pub folders: Vec<FolderSize>,
    pub largest: Vec<FileEntry>,
    #[serde(default)]
    pub types: Vec<TypeEntry>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    /// Relative to the scanned folder, `/` between names.
    pub path: String,
    pub size: u64,
    pub allocated: u64,
    #[serde(default)]
    pub modified: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypeEntry {
    pub extension: String,
    pub files: u64,
    pub size: u64,
}

/// Android's apps and how storage divides up.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppsReport {
    pub taken: i64,
    #[serde(default)]
    pub breakdown: Option<Breakdown>,
    pub apps: Vec<AppEntry>,
    /// Junk the device found (trash, old installers, …).
    #[serde(default)]
    pub junk: Vec<JunkEntry>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Breakdown {
    pub total: u64,
    pub free: u64,
    pub apps: u64,
    pub files: u64,
    pub system: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppEntry {
    pub package: String,
    pub label: String,
    pub code: u64,
    pub data: u64,
    pub cache: u64,
    #[serde(default)]
    pub last_used: Option<i64>,
    #[serde(default)]
    pub installed: Option<i64>,
    /// "Play Store", "downloaded file", …
    #[serde(default)]
    pub source: String,
    /// Whether FreeSize would suggest it for removal at all.
    #[serde(default)]
    pub removable: bool,
}

/// What a device's junk cleaner found, rule by rule.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JunkReport {
    pub taken: i64,
    /// Everything found.
    pub total: u64,
    /// The part that's safe and not in use: what one Clean would free.
    pub safe: u64,
    pub entries: Vec<JunkFinding>,
}

/// One rule's findings on a device.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct JunkFinding {
    /// The rule's id, which [`Action::CleanJunk`] names.
    pub kind: String,
    /// `system`, `browsers`, `apps`, `games`, `developer` or `trash`.
    pub category: String,
    pub label: String,
    /// What cleaning it means.
    pub blurb: String,
    pub bytes: u64,
    pub items: u64,
    /// `safe`, `check_first` or `use_tool`.
    pub safety: String,
    /// Comes back within days.
    pub refills: bool,
    /// Its program is running, so it's skipped until closed.
    #[serde(default)]
    pub running: bool,
    /// For `use_tool`: the tool to use instead.
    #[serde(default)]
    pub tool: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JunkEntry {
    /// Kind id the device understands in [`Action::CleanJunk`].
    pub kind: String,
    pub label: String,
    pub bytes: u64,
    pub items: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollRequest {
    /// How long the Hub may hold the request open waiting for something new,
    /// in seconds (at most 60).
    pub wait: u32,
    /// Sends everything still open again, not just what's new: set on the
    /// first poll after the device starts, so requests shown before a
    /// restart come back.
    #[serde(default)]
    pub resend: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollResponse {
    /// Commands for this device, signed by the Hub.
    #[serde(default)]
    pub commands: Vec<Signed<Command>>,
    /// Commands for other devices this device is the approver of.
    #[serde(default)]
    pub approvals: Vec<ApprovalRequest>,
    /// Decisions of this device's approver.
    #[serde(default)]
    pub decisions: Vec<Signed<Decision>>,
    /// Commands an admin withdrew: drop them if they're still waiting.
    /// (Unsigned: withdrawing can only stop an action.)
    #[serde(default)]
    pub cancelled: Vec<String>,
    /// The Hub's address has changed (it serves HTTPS now): move there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_to: Option<Signed<HubAddress>>,
    /// A newer FreeSize release the Hub knows of (with its update check
    /// on), for devices running an older version. Only a hint: the device
    /// still verifies the release's signature before installing anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<LatestRelease>,
}

/// A release newer than the device's version.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatestRelease {
    /// "0.9.1".
    pub version: String,
    /// The release's page.
    pub url: String,
}

/// The admin a command comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Admin {
    /// Stable id from the sign-in provider (OpenID `sub`).
    pub subject: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Command {
    pub id: String,
    /// The device it's for.
    pub device: String,
    pub action: Action,
    pub by: Admin,
    #[serde(default)]
    pub reason: String,
    pub created: i64,
    /// After this the device ignores it.
    pub expires: i64,
    /// Grant lengths the person approving may pick, in minutes.
    #[serde(default)]
    pub grant_minutes: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    /// Delete files and folders, given relative to a scanned folder.
    Delete {
        root: String,
        paths: Vec<String>,
        /// Their size when last scanned.
        bytes: u64,
    },
    /// Uninstall an Android app. Android asks for confirmation as well.
    Uninstall { package: String, label: String },
    /// Delete one kind of junk the device reported.
    CleanJunk {
        kind: String,
        label: String,
        bytes: u64,
    },
    /// Put a quarantined batch back where it was.
    Restore { batch: String, label: String },
    /// Delete quarantined batches for good.
    EmptyQuarantine { batches: Vec<String>, bytes: u64 },
    /// Remove Docker images or build cache the device reported as safe or
    /// unused.
    DockerCleanup {
        items: Vec<String>,
        labels: Vec<String>,
        bytes: u64,
    },
    /// Free the local copies of synced files (the Nextcloud client's
    /// virtual files on Windows, like Explorer's "Free up space"), or with
    /// `keep` bring them back to the device. The files stay listed; the
    /// client frees or fetches them on its next run.
    OnlineOnly {
        /// The sync folder on the device, as it reported it.
        root: String,
        /// Folders or files below it with `/` between names; "" for the
        /// whole folder.
        paths: Vec<String>,
        /// What it frees, or fetches.
        bytes: u64,
        #[serde(default)]
        keep: bool,
    },
    /// Move a scanned folder into the Nextcloud client's sync folder, so
    /// the client uploads it: a rename on the same drive, otherwise copied,
    /// checked byte for byte and the original moved to the quarantine.
    MoveIn {
        /// The scanned folder the source is in, as the device writes it.
        root: String,
        /// The folder to move, relative to `root`.
        path: String,
        /// The sync folder on the device, as it reported it.
        into: String,
        /// Where it goes below the sync folder, with `/` between names:
        /// `Archive/Desktop/GoPro`.
        target: String,
        bytes: u64,
    },
    /// On Unraid, add a share the Nextcloud container already mounts to
    /// Nextcloud as external storage (`occ files_external:create` with the
    /// `local` backend, in the container), so every device reaches it
    /// without a second copy.
    ExternalStorage {
        share: String,
        container: String,
        /// The folder name in Nextcloud, e.g. `/Media`.
        mount: String,
        /// The share's path inside the container.
        inside: String,
    },
}

impl Action {
    pub fn kind(&self) -> ActionKind {
        match self {
            Action::Delete { .. } => ActionKind::Delete,
            Action::Uninstall { .. } => ActionKind::Uninstall,
            Action::CleanJunk { .. } => ActionKind::Clean,
            Action::Restore { .. } => ActionKind::Restore,
            Action::EmptyQuarantine { .. } => ActionKind::Empty,
            Action::DockerCleanup { .. } => ActionKind::Docker,
            Action::OnlineOnly { .. } => ActionKind::OnlineOnly,
            Action::MoveIn { .. } => ActionKind::MoveIn,
            Action::ExternalStorage { .. } => ActionKind::Mount,
        }
    }

    /// One line saying what happens, for prompts and logs.
    pub fn summary(&self) -> String {
        match self {
            Action::Delete { root, paths, bytes } => format!(
                "Delete {} in {root} ({})",
                match paths.len() {
                    1 => paths[0].clone(),
                    n => format!("{n} items"),
                },
                freesize_core::format_size(*bytes)
            ),
            Action::Uninstall { label, package } => format!("Uninstall {label} ({package})"),
            Action::CleanJunk { label, bytes, .. } => {
                format!("{label} ({})", freesize_core::format_size(*bytes))
            }
            Action::Restore { label, .. } => format!("Put back {label}"),
            Action::EmptyQuarantine { batches, bytes } => format!(
                "Empty {} from the quarantine ({})",
                match batches.len() {
                    1 => "1 batch".to_string(),
                    n => format!("{n} batches"),
                },
                freesize_core::format_size(*bytes)
            ),
            Action::DockerCleanup { labels, bytes, .. } => format!(
                "Remove {} from Docker ({})",
                match labels.len() {
                    1 => labels[0].clone(),
                    n => format!("{n} items"),
                },
                freesize_core::format_size(*bytes)
            ),
            Action::OnlineOnly {
                root,
                paths,
                bytes,
                keep,
            } => format!(
                "{} {} in {root} ({})",
                if *keep {
                    "Keep on the device"
                } else {
                    "Make online-only"
                },
                match paths.as_slice() {
                    [] => "the folder".to_string(),
                    [one] if one.is_empty() => "the folder".to_string(),
                    [one] => one.clone(),
                    many => format!("{} items", many.len()),
                },
                freesize_core::format_size(*bytes)
            ),
            Action::MoveIn {
                root,
                path,
                target,
                bytes,
                ..
            } => format!(
                "Move {}{}{path} into the Nextcloud folder as {target} ({})",
                root.trim_end_matches(['/', '\\']),
                if root.contains('\\') { '\\' } else { '/' },
                freesize_core::format_size(*bytes)
            ),
            Action::ExternalStorage {
                share,
                container,
                mount,
                ..
            } => format!("Add the share {share} to Nextcloud ({container}) as {mount}"),
        }
    }
}

/// Where a command stands, as the device reports it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandState {
    /// Waiting for someone at the device.
    Shown,
    /// Waiting for the designated approver.
    NeedsApprover,
    Denied,
    Expired,
    Running,
    Done,
    Failed,
}

impl CommandState {
    pub fn finished(self) -> bool {
        matches!(
            self,
            CommandState::Denied
                | CommandState::Expired
                | CommandState::Done
                | CommandState::Failed
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandUpdate {
    pub state: CommandState,
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub approval: Option<Approval>,
    /// Bytes freed.
    #[serde(default)]
    pub freed: u64,
    /// Items that couldn't be done, with why.
    #[serde(default)]
    pub failed: Vec<String>,
}

/// How a command was allowed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    pub by: ApprovedBy,
    /// Length of the grant given with it, 0 for this command only.
    pub minutes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ApprovedBy {
    /// Someone at the device.
    Device,
    /// The designated approver, from their device.
    Approver { fingerprint: String },
    /// A grant given earlier.
    Grant { expires: i64 },
}

/// A command for another device, passed to its approver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub command: Signed<Command>,
    pub device: String,
    pub device_name: String,
    /// What the device says about it, e.g. how it deletes and what that
    /// really frees.
    #[serde(default)]
    pub note: String,
}

/// An approver's answer, signed by the approver's device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decision {
    pub command: String,
    /// [`Signed::hash`] of the command, so the decision fits only it.
    pub command_hash: String,
    /// The device the command is for.
    pub device: String,
    pub allow: bool,
    /// Grant length, 0 for this command only.
    pub minutes: u32,
    pub time: i64,
}

/// Another device's key, for choosing it as approver.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceKey {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    pub key: PublicKey,
}

/// An error answer from the Hub.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
    /// The device was removed from the Hub; it should forget the pairing.
    #[serde(default)]
    pub removed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::KeyPair;
    use crate::signing::Purpose;

    #[test]
    fn actions_are_tagged() {
        let action = Action::Delete {
            root: "/storage/emulated/0".into(),
            paths: vec!["Movies/a.mp4".into()],
            bytes: 2 * 1024 * 1024,
        };
        let text = serde_json::to_string(&action).unwrap();
        assert!(text.starts_with(r#"{"type":"delete","root":"#));
        assert_eq!(serde_json::from_str::<Action>(&text).unwrap(), action);
        assert_eq!(action.kind(), ActionKind::Delete);
        assert_eq!(
            action.summary(),
            "Delete Movies/a.mp4 in /storage/emulated/0 (2.00 MB)"
        );
    }

    #[test]
    fn old_and_new_fields_are_tolerated() {
        let info: DeviceInfo =
            serde_json::from_str(r#"{"name":"Pixel","platform":"android","future":1}"#).unwrap();
        assert_eq!(info.platform, Platform::Android);
        assert!(info.actions.is_empty());
        let odd: DeviceInfo = serde_json::from_str(r#"{"name":"x","platform":"beos"}"#).unwrap();
        assert_eq!(odd.platform, Platform::Other);
        let report: Report = serde_json::from_str("{}").unwrap();
        assert_eq!(report, Report::default());
    }

    #[test]
    fn signed_commands_travel() {
        let hub = KeyPair::from_seed([9; 32]);
        let command = Command {
            id: "C1".into(),
            device: "D1".into(),
            action: Action::Uninstall {
                package: "com.example".into(),
                label: "Example".into(),
            },
            by: Admin {
                subject: "abc".into(),
                name: "Robert".into(),
            },
            reason: String::new(),
            created: 1,
            expires: 2,
            grant_minutes: vec![10, 30],
        };
        let poll = PollResponse {
            commands: vec![Signed::sign(&command, &hub, Purpose::Command)],
            ..PollResponse::default()
        };
        let text = serde_json::to_string(&poll).unwrap();
        let back: PollResponse = serde_json::from_str(&text).unwrap();
        assert_eq!(
            back.commands[0].open(&hub.public(), Purpose::Command),
            Ok(command)
        );
    }
}
