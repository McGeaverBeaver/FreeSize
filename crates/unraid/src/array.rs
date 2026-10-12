//! The array and pools, from Unraid's own state (`disks.ini`), so nothing
//! has to touch a disk to learn about it, and sleeping disks stay asleep.

use disktree_proto::unraid::{ArrayDisk, DiskKind};

use crate::System;
use crate::ini::{parse_cfg, parse_ini};

/// A mounted filesystem, from `/proc/mounts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mount {
    pub device: String,
    pub path: String,
    pub fs: String,
}

/// `/proc/mounts`, with its octal escapes (`\040` for a space) undone.
pub fn mounts(system: &System) -> Vec<Mount> {
    let Some(text) = system.read("/proc/mounts") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split(' ');
            let device = unescape(fields.next()?);
            let path = unescape(fields.next()?);
            let fs = fields.next()?.to_string();
            Some(Mount { device, path, fs })
        })
        .collect()
}

fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(code) = u8::from_str_radix(&field[i + 1..i + 4], 8)
        {
            out.push(code);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The array as Unraid sees it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Array {
    pub version: String,
    pub started: bool,
    pub disks: Vec<ArrayDisk>,
}

fn kilobytes(map: &std::collections::BTreeMap<String, String>, key: &str) -> u64 {
    map.get(key)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0)
        .saturating_mul(1024)
}

/// Reads the array from `disks.ini`, `var.ini` and the mounts.
pub fn read(system: &System) -> Array {
    let state = system
        .read("/var/local/emhttp/var.ini")
        .map(|t| parse_cfg(&t))
        .unwrap_or_default();
    let started = state.get("mdState").is_some_and(|s| s == "STARTED");
    let mounts = mounts(system);
    let mounted = |path: &str| mounts.iter().find(|m| m.path == path);
    let mut disks = Vec::new();
    let sections = system
        .read("/var/local/emhttp/disks.ini")
        .map(|t| parse_ini(&t))
        .unwrap_or_default();
    for (section, map) in sections {
        let name = map.get("name").cloned().unwrap_or(section);
        let device = map.get("device").cloned().unwrap_or_default();
        // Empty slots.
        if device.is_empty() {
            continue;
        }
        let kind = match map.get("type").map(String::as_str) {
            Some("Parity") => DiskKind::Parity,
            Some("Data") => DiskKind::Data,
            Some("Cache") => DiskKind::Pool,
            Some("Flash") => DiskKind::Flash,
            _ => continue,
        };
        let mount_path = match kind {
            DiskKind::Parity => None,
            DiskKind::Flash => Some("/boot".to_string()),
            DiskKind::Data | DiskKind::Pool => Some(format!("/mnt/{name}")),
        };
        let mount = mount_path.as_deref().and_then(mounted);
        // A pool's first device names the pool and is where it's mounted;
        // the others ("cache2", …) belong to it.
        let pool = (kind == DiskKind::Pool && mount.is_none())
            .then(|| {
                name.trim_end_matches(|c: char| c.is_ascii_digit())
                    .to_string()
            })
            .filter(|p| !p.is_empty() && *p != name);
        let fs = map
            .get("fsType")
            .filter(|f| !f.is_empty())
            .cloned()
            .or_else(|| mount.map(|m| m.fs.clone()))
            .unwrap_or_default();
        let (size, free) = match kilobytes(&map, "fsSize") {
            0 => (kilobytes(&map, "size"), 0),
            size => (size, kilobytes(&map, "fsFree")),
        };
        let temp = map.get("temp").and_then(|t| t.trim().parse::<i32>().ok());
        disks.push(ArrayDisk {
            name,
            kind,
            device,
            path: mount.map(|m| m.path.clone()),
            fs,
            size,
            free,
            asleep: map.get("spundown").is_some_and(|s| s == "1"),
            temp,
            status: map.get("status").cloned().unwrap_or_default(),
            scanned: None,
            pool,
        });
    }
    Array {
        version: system.version(),
        started,
        disks,
    }
}

impl Array {
    pub fn disk(&self, name: &str) -> Option<&ArrayDisk> {
        self.disks.iter().find(|d| d.name == name)
    }

    /// Disks and pools holding files, mounted: what a scan covers. Sleeping
    /// ones are left out unless `wake` is set.
    pub fn roots(&self, wake: bool) -> Vec<&ArrayDisk> {
        self.disks
            .iter()
            .filter(|d| matches!(d.kind, DiskKind::Data | DiskKind::Pool))
            .filter(|d| d.path.is_some() && (wake || !d.asleep))
            .collect()
    }

    /// Every disk or pool a share can have files on, with its mount.
    pub fn file_disks(&self) -> Vec<(&str, &str)> {
        self.disks
            .iter()
            .filter(|d| matches!(d.kind, DiskKind::Data | DiskKind::Pool))
            .filter_map(|d| d.path.as_deref().map(|p| (d.name.as_str(), p)))
            .collect()
    }

    /// Where a path under `/mnt/user` can physically be: the same path on
    /// each disk and pool, as `(disk, path)`. Paths already on a disk come
    /// back as they are.
    pub fn physical(&self, path: &str) -> Vec<(String, String)> {
        let path = path.trim_end_matches('/');
        if let Some(rest) = path
            .strip_prefix("/mnt/user/")
            .or_else(|| path.strip_prefix("/mnt/user0/"))
        {
            let array_only = path.starts_with("/mnt/user0/");
            return self
                .disks
                .iter()
                .filter(|d| d.kind == DiskKind::Data || (!array_only && d.kind == DiskKind::Pool))
                .filter_map(|d| {
                    d.path
                        .as_deref()
                        .map(|mount| (d.name.clone(), format!("{mount}/{rest}")))
                })
                .collect();
        }
        self.disks
            .iter()
            .filter_map(|d| {
                let mount = d.path.as_deref()?;
                (path == mount || path.starts_with(&format!("{mount}/")))
                    .then(|| (d.name.clone(), path.to_string()))
            })
            .collect()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A server with parity, two data disks (disk2 asleep), a two-device
    /// cache pool and the flash drive.
    pub fn server() -> (tempfile::TempDir, System) {
        let dir = tempfile::tempdir().unwrap();
        let system = System::at(dir.path());
        let write = |path: &str, text: &str| {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        };
        write("/etc/unraid-version", "version=\"7.1.2\"\n");
        write(
            "/var/local/emhttp/var.ini",
            "version=\"7.1.2\"\nmdState=\"STARTED\"\nfsState=\"Started\"\n",
        );
        write(
            "/var/local/emhttp/disks.ini",
            r#"["parity"]
idx="0"
name="parity"
device="sdb"
id="ST8000VN004-2M2101_WSD1ABCD"
size="7814026532"
status="DISK_OK"
temp="31"
type="Parity"
spundown="0"
fsType=""
["disk1"]
idx="1"
name="disk1"
device="sdc"
size="7814026532"
status="DISK_OK"
temp="33"
type="Data"
spundown="0"
fsStatus="Mounted"
fsType="xfs"
fsSize="7811939620"
fsFree="1953484905"
fsUsed="5858454715"
["disk2"]
idx="2"
name="disk2"
device="sdd"
size="3907018532"
status="DISK_OK"
temp="*"
type="Data"
spundown="1"
fsStatus="Mounted"
fsType="xfs"
fsSize="3905961924"
fsFree="3000000000"
["disk3"]
idx="3"
name="disk3"
device=""
status="DISK_NP"
type="Data"
["cache"]
idx="30"
name="cache"
device="nvme0n1"
size="976762552"
status="DISK_OK"
temp="41"
type="Cache"
spundown="0"
fsStatus="Mounted"
fsType="btrfs"
fsSize="976762552"
fsFree="500000000"
["cache2"]
idx="31"
name="cache2"
device="nvme1n1"
size="976762552"
status="DISK_OK"
temp="40"
type="Cache"
spundown="0"
fsType=""
["flash"]
idx="54"
name="flash"
device="sda"
size="31266784"
status="DISK_OK"
type="Flash"
fsType="vfat"
fsSize="31266784"
fsFree="30000000"
"#,
        );
        write(
            "/proc/mounts",
            "/dev/sda1 /boot vfat rw,noatime 0 0\n\
             /dev/md1p1 /mnt/disk1 xfs rw,noatime 0 0\n\
             /dev/md2p1 /mnt/disk2 xfs rw,noatime 0 0\n\
             /dev/nvme0n1p1 /mnt/cache btrfs rw,noatime 0 0\n\
             shfs /mnt/user fuse.shfs rw,nosuid,nodev,noatime 0 0\n\
             /dev/sde1 /mnt/disks/Old\\040Drive ext4 rw 0 0\n",
        );
        for dir in ["/mnt/disk1", "/mnt/disk2", "/mnt/cache"] {
            std::fs::create_dir_all(system.file(dir)).unwrap();
        }
        (dir, system)
    }

    #[test]
    fn reads_the_array() {
        let (_dir, system) = server();
        assert!(system.is_unraid());
        let array = read(&system);
        assert_eq!(array.version, "7.1.2");
        assert!(array.started);
        let names: Vec<&str> = array.disks.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(
            names,
            ["parity", "disk1", "disk2", "cache", "cache2", "flash"]
        );
        let disk1 = array.disk("disk1").unwrap();
        assert_eq!(disk1.kind, DiskKind::Data);
        assert_eq!(disk1.path.as_deref(), Some("/mnt/disk1"));
        assert_eq!(disk1.size, 7_811_939_620 * 1024);
        assert_eq!(disk1.free, 1_953_484_905 * 1024);
        assert_eq!(disk1.temp, Some(33));
        assert!(!disk1.asleep);
        let disk2 = array.disk("disk2").unwrap();
        assert!(disk2.asleep);
        assert_eq!(disk2.temp, None);
        let cache2 = array.disk("cache2").unwrap();
        assert_eq!(cache2.pool.as_deref(), Some("cache"));
        assert_eq!(cache2.path, None);
        assert_eq!(array.disk("parity").unwrap().path, None);
        assert_eq!(array.disk("flash").unwrap().path.as_deref(), Some("/boot"));

        let roots: Vec<&str> = array.roots(false).iter().map(|d| d.name.as_str()).collect();
        assert_eq!(roots, ["disk1", "cache"]);
        assert_eq!(array.roots(true).len(), 3);
    }

    #[test]
    fn user_paths_map_to_disks() {
        let (_dir, system) = server();
        let array = read(&system);
        let places = array.physical("/mnt/user/Movies/Dune");
        assert_eq!(
            places,
            [
                ("disk1".to_string(), "/mnt/disk1/Movies/Dune".to_string()),
                ("disk2".to_string(), "/mnt/disk2/Movies/Dune".to_string()),
                ("cache".to_string(), "/mnt/cache/Movies/Dune".to_string()),
            ]
        );
        // user0 is the array without pools.
        assert_eq!(array.physical("/mnt/user0/Movies").len(), 2);
        assert_eq!(
            array.physical("/mnt/disk2/Movies"),
            [("disk2".to_string(), "/mnt/disk2/Movies".to_string())]
        );
        assert!(array.physical("/mnt/remotes/nas/x").is_empty());
    }

    #[test]
    fn mount_names_are_unescaped() {
        let (_dir, system) = server();
        let mounts = mounts(&system);
        assert!(mounts.iter().any(|m| m.path == "/mnt/disks/Old Drive"));
    }
}
