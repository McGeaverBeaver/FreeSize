//! One round of looking at the server: scan the disks that are awake (and
//! one that slept through too many rounds), then work out the shares,
//! Docker, VMs, permissions and what must never be deleted.

use std::collections::BTreeMap;

use freesize_core::{ScanOptions, ScanProgress, scan};
use freesize_proto::ScanReport;
use freesize_proto::unraid::{DiskKind, UnraidReport};

use crate::array::{self, Array};
use crate::docker::{self, Inventory};
use crate::protect::{self, Rule};
use crate::scans::{DiskScan, Scans};
use crate::shares::{self, DiskTops};
use crate::{System, nextcloud, perms, vms};

#[derive(Clone, Debug)]
pub struct Settings {
    /// Wake a sleeping disk whose last scan is older than this many days;
    /// 0 never wakes one.
    pub wake_days: u32,
    /// Look for files with odd owners or modes.
    pub permissions: bool,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            wake_days: 7,
            permissions: true,
        }
    }
}

/// What a round found.
pub struct Round {
    pub taken: i64,
    pub array: Array,
    /// Why nothing was scanned, if it wasn't (the array is stopped).
    pub skipped: Option<String>,
    /// One per scanned disk, for the Hub.
    pub scans: Vec<ScanReport>,
    /// Each disk's share sizes, this round's or earlier ones.
    pub tops: BTreeMap<String, DiskTops>,
    /// Everything but activity and moves, which the daemon adds.
    pub report: UnraidReport,
    /// What must never be deleted, as of now.
    pub rules: Vec<Rule>,
    pub inventory: Option<Inventory>,
    pub errors: Vec<String>,
}

/// Which disks to scan this round: the awake ones, plus sleeping ones
/// never scanned or not for `wake_days`.
pub fn choose(
    array: &Array,
    tops: &BTreeMap<String, DiskTops>,
    settings: &Settings,
    now: i64,
) -> Vec<String> {
    array
        .roots(true)
        .iter()
        .filter(|disk| {
            if !disk.asleep {
                return true;
            }
            if settings.wake_days == 0 {
                return false;
            }
            match tops.get(&disk.name) {
                Some(t) => now - t.scanned > i64::from(settings.wake_days) * 86_400,
                None => true,
            }
        })
        .map(|d| d.name.clone())
        .collect()
}

pub fn round(
    system: &System,
    settings: &Settings,
    tops: &BTreeMap<String, DiskTops>,
    now: i64,
    progress: &dyn Fn(&str),
) -> Round {
    let array = array::read(system);
    let mut out = Round {
        taken: now,
        array: array.clone(),
        skipped: None,
        scans: Vec::new(),
        tops: tops.clone(),
        report: UnraidReport {
            taken: now,
            version: array.version.clone(),
            started: array.started,
            ..UnraidReport::default()
        },
        rules: Vec::new(),
        inventory: None,
        errors: Vec::new(),
    };
    let docker_cfg = docker::config(system);
    let vm_cfg = vms::config(system);
    if !array.started {
        out.skipped = Some("the array is stopped".into());
        // Nothing is mounted: keep what's known, protect everything.
        out.report.disks = disks_with_scans(&array, &out.tops);
        out.rules = protect::rules(&array, &[], (&docker_cfg, None), (&vm_cfg, None));
        return out;
    }

    // What the later steps look at file by file; every other file is let
    // go with its disk's tree, so a big array's files never sit in memory
    // all at once.
    let definitions = vms::definitions(system);
    let instances = nextcloud::find(system, &array);
    let mut keep = nextcloud::keep(&instances);
    keep.under.push(format!("/mnt/user/{}", vm_cfg.isos));
    keep.under.push(format!("/mnt/user/{}", vm_cfg.domains));
    keep.in_folder
        .push(format!("/mnt/user/{}", docker_cfg.appdata));
    for def in definitions.iter().flatten() {
        keep.files
            .extend(def.disks.iter().chain(&def.isos).map(|p| vms::user_path(p)));
    }

    // Scan.
    let mut scans = Scans::default();
    let chosen = choose(&array, tops, settings, now);
    let total = chosen.len();
    for (i, name) in chosen.into_iter().enumerate() {
        let Some(disk) = array.disk(&name) else {
            continue;
        };
        let Some(mount) = disk.path.clone() else {
            continue;
        };
        progress(&format!("scanning {name} ({} of {total})", i + 1));
        // ZFS datasets and btrfs subvolumes are filesystems of their own,
        // inside the pool.
        let options = ScanOptions {
            one_filesystem: !matches!(disk.fs.as_str(), "zfs" | "btrfs"),
            ..ScanOptions::default()
        };
        match scan(system.file(&mount), &options, &ScanProgress::new()) {
            Ok(result) => {
                let tree = result.tree;
                out.tops.insert(
                    name.clone(),
                    DiskTops {
                        disk: name.clone(),
                        scanned: now,
                        tops: shares::tops_of(&tree),
                    },
                );
                let mut report =
                    freesize_agent::report::scan_report(&tree, &name, now, result.errors, true);
                report.root = mount.clone();
                out.scans.push(report);
                scans
                    .disks
                    .push(DiskScan::compact(&name, &mount, &tree, now, &keep));
                drop(tree);
            }
            Err(err) => out.errors.push(format!("{name}: {err}")),
        }
    }

    // Shares.
    let configs = shares::read(system);
    let scanned: Vec<&str> = scans.disks.iter().map(|d| d.disk.as_str()).collect();
    let fresh: Vec<DiskTops> = out
        .tops
        .values()
        .filter(|t| scanned.contains(&t.disk.as_str()))
        .cloned()
        .collect();
    let mounted: Vec<&str> = array.file_disks().iter().map(|(d, _)| *d).collect();
    let stale: Vec<DiskTops> = out
        .tops
        .values()
        .filter(|t| !scanned.contains(&t.disk.as_str()) && mounted.contains(&t.disk.as_str()))
        .cloned()
        .collect();
    out.report.shares = shares::distribution(&configs, &fresh, &stale);
    out.report.disks = disks_with_scans(&array, &out.tops);

    // Docker and VMs.
    progress("looking at Docker and VMs");
    let engine = docker::Engine::new(system);
    let inventory = if docker_cfg.enabled || engine.available() {
        docker::inventory(&engine, system)
    } else {
        Err("Docker is off".into())
    };
    if let Err(err) = &inventory
        && docker_cfg.enabled
    {
        out.errors.push(format!("Docker: {err}"));
    }
    out.inventory = inventory.as_ref().ok().cloned();
    let templates = docker::templates(system);
    if docker_cfg.enabled || out.inventory.is_some() {
        out.report.docker = Some(docker::report(
            system,
            &docker_cfg,
            inventory,
            &templates,
            &array,
            &scans,
        ));
    }
    if vm_cfg.enabled || definitions.is_some() {
        out.report.vms = Some(vms::report(system, &vm_cfg, &array, &scans));
    }
    if !instances.is_empty() {
        progress("looking at Nextcloud");
        out.report.nextcloud = instances
            .iter()
            .filter_map(|instance| nextcloud::report(system, &array, &scans, instance, now))
            .collect();
    }

    // Permissions, while the scan's details are still in memory.
    if settings.permissions {
        progress("checking permissions");
        out.report.permissions = perms::report(system, &array, &[]);
    }

    let share_names: Vec<String> = out.report.shares.iter().map(|s| s.name.clone()).collect();
    out.rules = protect::rules(
        &array,
        &share_names,
        (&docker_cfg, out.inventory.as_ref()),
        (&vm_cfg, definitions.as_deref()),
    );
    out.report.peak_memory = crate::peak_memory();
    out
}

/// The array's disks, each with when it was last scanned.
fn disks_with_scans(
    array: &Array,
    tops: &BTreeMap<String, DiskTops>,
) -> Vec<freesize_proto::unraid::ArrayDisk> {
    array
        .disks
        .iter()
        .map(|d| {
            let mut disk = d.clone();
            disk.scanned = tops.get(&d.name).map(|t| t.scanned);
            disk
        })
        .collect()
}

/// The disks as the Hub's device page shows them.
pub fn hub_disks(array: &Array) -> Vec<freesize_proto::Disk> {
    array
        .disks
        .iter()
        .filter(|d| d.kind != DiskKind::Parity && d.size > 0)
        .filter_map(|d| {
            Some(freesize_proto::Disk {
                name: d.name.clone(),
                path: d.path.clone()?,
                total: d.size,
                free: d.free,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::tests::server;

    #[test]
    fn a_round_scans_awake_disks_and_keeps_the_rest() {
        let (_dir, system) = server();
        for (path, bytes) in [
            ("/mnt/disk1/Movies/a.mkv", 40_000),
            ("/mnt/disk2/Movies/b.mkv", 10_000),
            ("/mnt/cache/appdata/plex/db", 5_000),
        ] {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, vec![1u8; bytes]).unwrap();
        }
        // disk2 slept through the last round, scanned two days ago.
        let mut tops = BTreeMap::new();
        tops.insert(
            "disk2".to_string(),
            DiskTops {
                disk: "disk2".into(),
                scanned: 1_000_000 - 2 * 86_400,
                tops: [("Movies".to_string(), 99_999u64)].into_iter().collect(),
            },
        );
        let settings = Settings {
            wake_days: 7,
            permissions: false,
        };
        let round = round(&system, &settings, &tops, 1_000_000, &|_| {});
        assert!(round.skipped.is_none());
        let roots: Vec<&str> = round.scans.iter().map(|s| s.root.as_str()).collect();
        assert_eq!(roots, ["/mnt/disk1", "/mnt/cache"]);
        assert_eq!(round.scans[0].label, "disk1");
        let movies = round
            .report
            .shares
            .iter()
            .find(|s| s.name == "Movies")
            .unwrap();
        assert!(!movies.complete);
        assert!(
            movies
                .placements
                .iter()
                .any(|p| p.disk == "disk2" && p.stale && p.bytes == 99_999)
        );
        let disk2 = round
            .report
            .disks
            .iter()
            .find(|d| d.name == "disk2")
            .unwrap();
        assert_eq!(disk2.scanned, Some(1_000_000 - 2 * 86_400));
        // Docker is off and there are no VMs: nothing reported for them,
        // and appdata is protected outright.
        assert!(round.report.docker.is_none());
        assert!(protect::check(&round.rules, "/mnt/cache/appdata/plex/db").is_some());
        assert_eq!(hub_disks(&round.array).len(), 4);

        // A week later the sleeping disk is woken once.
        let woken = choose(&round.array, &round.tops, &settings, 1_000_000 + 8 * 86_400);
        assert!(woken.contains(&"disk2".to_string()));
        let never = Settings {
            wake_days: 0,
            ..settings
        };
        assert!(
            !choose(&round.array, &round.tops, &never, 2_000_000).contains(&"disk2".to_string())
        );
    }

    #[test]
    fn a_stopped_array_is_left_alone() {
        let (_dir, system) = server();
        std::fs::write(
            system.file("/var/local/emhttp/var.ini"),
            "mdState=\"STOPPED\"\n",
        )
        .unwrap();
        let round = round(&system, &Settings::default(), &BTreeMap::new(), 5, &|_| {});
        assert_eq!(round.skipped.as_deref(), Some("the array is stopped"));
        assert!(round.scans.is_empty());
        assert!(protect::check(&round.rules, "/mnt/user/appdata/x").is_some());
    }
}
