//! What an Unraid server reports beyond plain disks and scans: the array,
//! where each share's files sit, Docker and VMs sorted by how safe they are
//! to clean, what keeps disks awake, file permissions and moves between
//! disks. Also the quarantine, which any server agent reports.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UnraidReport {
    /// Seconds since 1970.
    pub taken: i64,
    /// Unraid's version, e.g. "7.1.2".
    pub version: String,
    /// Whether the array is started; with it stopped, nothing is mounted.
    pub started: bool,
    pub disks: Vec<ArrayDisk>,
    pub shares: Vec<ShareUse>,
    #[serde(default)]
    pub docker: Option<DockerReport>,
    #[serde(default)]
    pub vms: Option<VmReport>,
    #[serde(default)]
    pub activity: Vec<DiskActivity>,
    #[serde(default)]
    pub permissions: Vec<PermissionIssue>,
    #[serde(default)]
    pub moves: Vec<MoveJob>,
    /// Nextcloud's data folders on the server, one report each.
    #[serde(default)]
    pub nextcloud: Vec<NextcloudReport>,
    /// The most memory the last round used, in bytes, where Linux says.
    #[serde(default)]
    pub peak_memory: Option<u64>,
}

/// Nextcloud's data folder: the people's files against previews, old
/// versions, the trash and uploads in progress, with the settings that
/// would have cleared them. Nothing in it is offered for deletion.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextcloudReport {
    /// The data folder, as a `/mnt/user` path.
    pub data: String,
    /// `config.php`, where it was found.
    #[serde(default)]
    pub config: String,
    #[serde(default)]
    pub version: String,
    /// The whole data folder, on disk.
    pub total: u64,
    pub users: Vec<NextcloudUser>,
    /// `appdata_*/preview`, and the rest of `appdata_*`.
    pub previews: u64,
    pub appdata: u64,
    /// Every user's uploads in progress.
    pub uploads: u64,
    /// `versions_retention_obligation` and `trashbin_retention_obligation`.
    pub versions_setting: String,
    pub trash_setting: String,
    /// Older than the settings' latest day, which the background job
    /// should have removed.
    pub versions_expired: u64,
    pub trash_expired: u64,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NextcloudUser {
    pub name: String,
    pub files: u64,
    pub versions: u64,
    pub trash: u64,
    pub uploads: u64,
    pub cache: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskKind {
    Parity,
    #[default]
    Data,
    /// A pool (cache and others), or a device in one.
    Pool,
    Flash,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArrayDisk {
    /// `disk1`, `parity`, `cache`.
    pub name: String,
    pub kind: DiskKind,
    /// The Linux device, e.g. `sdc` or `nvme0n1`.
    pub device: String,
    /// Where it's mounted (`/mnt/disk1`, `/mnt/cache`), if it holds files.
    #[serde(default)]
    pub path: Option<String>,
    /// xfs, btrfs, zfs, …
    pub fs: String,
    pub size: u64,
    pub free: u64,
    /// Spun down: not scanned, so its last scan stands.
    pub asleep: bool,
    /// °C, when Unraid knows it.
    #[serde(default)]
    pub temp: Option<i32>,
    /// Unraid's status, e.g. `DISK_OK`.
    pub status: String,
    /// When its files were last scanned.
    #[serde(default)]
    pub scanned: Option<i64>,
    /// The pool a pool device belongs to.
    #[serde(default)]
    pub pool: Option<String>,
}

/// One share and the disks its files are on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareUse {
    pub name: String,
    #[serde(default)]
    pub comment: String,
    /// Unraid's cache setting: `yes`, `no`, `only`, `prefer`, or the pool
    /// it uses.
    #[serde(default)]
    pub cache: String,
    pub total: u64,
    pub placements: Vec<Placement>,
    /// Every disk holding it was scanned this time (none was asleep).
    pub complete: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub disk: String,
    pub bytes: u64,
    /// From an earlier scan, because the disk was asleep.
    #[serde(default)]
    pub stale: bool,
}

/// How safe something is to clean up.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Safety {
    /// Rebuilt or downloaded again on its own: dangling images, build cache.
    Safe,
    /// Probably unneeded, but look first: you may want it back.
    #[default]
    Check,
    /// In use by a container or VM, or part of the system: never offered.
    Protected,
}

/// Something found in Docker or among the VMs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable, e.g. `image:sha256:…`, `appdata:plex`, `build_cache`.
    pub id: String,
    /// image, dangling_image, build_cache, volume, appdata, log, layer,
    /// vdisk, iso, snapshot.
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub path: Option<String>,
    pub bytes: u64,
    pub safety: Safety,
    /// Why it's in that class, e.g. "used by plex (running)".
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DockerReport {
    /// Whether the Docker service runs.
    pub running: bool,
    /// `image` (docker.img) or `folder` (directory mode).
    pub mode: String,
    /// docker.img, or the folder.
    pub location: String,
    /// The image's size and what's used inside it.
    #[serde(default)]
    pub image_size: u64,
    #[serde(default)]
    pub image_used: u64,
    pub containers: u32,
    pub running_containers: u32,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmReport {
    /// Whether the VM service is on.
    pub enabled: bool,
    pub vms: Vec<Vm>,
    pub findings: Vec<Finding>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Vm {
    pub name: String,
    pub running: bool,
    pub disks: Vec<VmDisk>,
    /// ISOs in its CD drives.
    pub isos: Vec<String>,
    pub snapshots: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmDisk {
    pub path: String,
    /// The size the VM sees, and what it takes on disk (less for sparse
    /// files); unknown while its disk sleeps.
    #[serde(default)]
    pub provisioned: Option<u64>,
    #[serde(default)]
    pub used: Option<u64>,
}

/// Items moved aside together, to put back or empty.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineBatch {
    pub id: String,
    /// Seconds since 1970.
    pub when: i64,
    /// Who asked: an admin's name, or "Unraid web UI".
    pub by: String,
    #[serde(default)]
    pub reason: String,
    pub bytes: u64,
    pub items: Vec<QuarantineItem>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineItem {
    /// Where it was.
    pub original: String,
    /// Where it is now (on the same disk).
    pub stored: String,
    pub bytes: u64,
}

/// What a disk did over the last day.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskActivity {
    pub disk: String,
    /// Times it spun up.
    pub wakeups: u32,
    pub awake_minutes: u32,
    /// Sectors read and written.
    pub reads: u64,
    pub writes: u64,
    /// Files open on it at the last look.
    pub open: Vec<OpenFile>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    pub path: String,
    pub process: String,
    #[serde(default)]
    pub container: Option<String>,
}

/// Files in a share whose owner or mode can break access over SMB.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionIssue {
    pub share: String,
    pub count: u64,
    /// A few of them, as "path (owner uid:gid, mode 0700)".
    pub examples: Vec<String>,
}

/// A folder moved from one disk to another.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveJob {
    pub id: String,
    pub share: String,
    /// Relative to the share; empty for all of it.
    pub folder: String,
    pub from: String,
    pub to: String,
    pub bytes: u64,
    pub files: u64,
    /// planned, running, done, failed, cancelled.
    pub state: String,
    pub done_bytes: u64,
    pub done_files: u64,
    #[serde(default)]
    pub detail: String,
    pub started: i64,
    #[serde(default)]
    pub finished: Option<i64>,
    /// The file being copied, below the moved folder.
    #[serde(default)]
    pub current: String,
    /// Delete the originals once every copy checked out, rather than
    /// leaving them in the source disk's quarantine.
    #[serde(default)]
    pub empty: bool,
}
