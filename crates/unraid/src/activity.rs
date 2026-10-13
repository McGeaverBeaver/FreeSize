//! What keeps disks spun up: each minute, whether each disk sleeps and how
//! much it read and wrote (`/proc/diskstats`); and at each look, which
//! files are open on it and by which process or container.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;

use freesize_proto::unraid::{DiskActivity, DiskKind, OpenFile};

use crate::System;
use crate::array::Array;

/// A day of samples, one a minute.
const KEEP: usize = 24 * 60;
/// Open files listed per disk at most.
const MAX_OPEN: usize = 20;

/// One disk at one moment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct DiskSample {
    asleep: bool,
    /// Sectors read and written since boot.
    reads: u64,
    writes: u64,
}

#[derive(Clone, Debug, Default)]
struct Sample {
    time: i64,
    disks: BTreeMap<String, DiskSample>,
}

/// Samples over the last day.
#[derive(Clone, Debug, Default)]
pub struct Monitor {
    samples: VecDeque<Sample>,
}

/// Sectors read and written per device, from `/proc/diskstats`.
pub fn diskstats(system: &System) -> BTreeMap<String, (u64, u64)> {
    let Some(text) = system.read("/proc/diskstats") else {
        return BTreeMap::new();
    };
    text.lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let reads = fields.get(5)?.parse().ok()?;
            let writes = fields.get(9)?.parse().ok()?;
            Some((fields.get(2)?.to_string(), (reads, writes)))
        })
        .collect()
}

impl Monitor {
    /// Takes a sample of every array disk and pool device.
    pub fn record(&mut self, array: &Array, stats: &BTreeMap<String, (u64, u64)>, time: i64) {
        let mut sample = Sample {
            time,
            disks: BTreeMap::new(),
        };
        for disk in &array.disks {
            if disk.kind == DiskKind::Flash {
                continue;
            }
            let (reads, writes) = stats.get(&disk.device).copied().unwrap_or_default();
            sample.disks.insert(
                disk.name.clone(),
                DiskSample {
                    asleep: disk.asleep,
                    reads,
                    writes,
                },
            );
        }
        self.samples.push_back(sample);
        while self.samples.len() > KEEP {
            self.samples.pop_front();
        }
    }

    /// The day so far, per disk, with what's open on each now.
    pub fn summary(&self, open: &BTreeMap<String, Vec<OpenFile>>) -> Vec<DiskActivity> {
        let Some(last) = self.samples.back() else {
            return Vec::new();
        };
        let minutes = |a: &Sample, b: &Sample| ((b.time - a.time) / 60).max(1) as u32;
        last.disks
            .keys()
            .map(|name| {
                let mut activity = DiskActivity {
                    disk: name.clone(),
                    open: open
                        .get(name)
                        .map(|list| list.iter().take(MAX_OPEN).cloned().collect())
                        .unwrap_or_default(),
                    ..DiskActivity::default()
                };
                let mut previous: Option<(&Sample, &DiskSample)> = None;
                for sample in &self.samples {
                    let Some(disk) = sample.disks.get(name) else {
                        continue;
                    };
                    if let Some((before_sample, before)) = previous {
                        if before.asleep && !disk.asleep {
                            activity.wakeups += 1;
                        }
                        if !before.asleep {
                            activity.awake_minutes += minutes(before_sample, sample);
                        }
                        // Counters reset when a device is reset or re-added.
                        activity.reads +=
                            disk.reads.checked_sub(before.reads).unwrap_or(disk.reads);
                        activity.writes += disk
                            .writes
                            .checked_sub(before.writes)
                            .unwrap_or(disk.writes);
                    }
                    previous = Some((sample, disk));
                }
                activity
            })
            .collect()
    }
}

/// The container a process runs in, from its cgroup: Docker's id.
fn container_of(system: &System, pid: u32) -> Option<String> {
    let cgroup = system.read(&format!("/proc/{pid}/cgroup"))?;
    cgroup.lines().find_map(|line| {
        let path = line.rsplit(':').next()?;
        let id = path.split('/').find_map(|part| {
            part.strip_prefix("docker-")
                .and_then(|p| p.strip_suffix(".scope"))
                .or_else(|| {
                    (part.len() == 64 && part.chars().all(|c| c.is_ascii_hexdigit()))
                        .then_some(part)
                })
        })?;
        Some(id.to_string())
    })
}

/// Files open now, by the disk they're on. `/mnt/user` paths are put on
/// the awake disks that hold them; `containers` names Docker's ids.
pub fn open_by_disk(
    system: &System,
    array: &Array,
    containers: &BTreeMap<String, String>,
) -> BTreeMap<String, Vec<OpenFile>> {
    let mut by_disk: BTreeMap<String, Vec<OpenFile>> = BTreeMap::new();
    let proc = system.file("/proc");
    for (path, process, pid) in freesize_agent::guard::open_files(&proc) {
        let server = system
            .unraid_path(&path)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        if !server.starts_with("/mnt/") {
            continue;
        }
        let container = container_of(system, pid).map(|id| {
            containers
                .iter()
                .find(|(full, _)| full.starts_with(&id) || id.starts_with(full.as_str()))
                .map_or_else(|| id.chars().take(12).collect(), |(_, name)| name.clone())
        });
        for (disk, physical) in array.physical(&server) {
            let awake = array.disk(&disk).is_some_and(|d| !d.asleep);
            if !awake || !Path::new(&system.file(&physical)).exists() {
                continue;
            }
            let list = by_disk.entry(disk).or_default();
            let file = OpenFile {
                path: server.clone(),
                process: process.clone(),
                container: container.clone(),
            };
            if !list.contains(&file) {
                list.push(file);
            }
        }
    }
    by_disk
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::{self, tests::server};

    #[test]
    fn counts_wakeups_and_io() {
        let (_dir, system) = server();
        let mut array = array::read(&system);
        let mut monitor = Monitor::default();
        let stats = |r: u64, w: u64| {
            [("sdd".to_string(), (r, w)), ("sdc".to_string(), (5, 5))]
                .into_iter()
                .collect::<BTreeMap<_, _>>()
        };
        let set_sleep = |array: &mut Array, asleep: bool| {
            array
                .disks
                .iter_mut()
                .find(|d| d.name == "disk2")
                .unwrap()
                .asleep = asleep;
        };
        set_sleep(&mut array, true);
        monitor.record(&array, &stats(100, 10), 0);
        set_sleep(&mut array, false);
        monitor.record(&array, &stats(300, 10), 60);
        monitor.record(&array, &stats(500, 50), 120);
        set_sleep(&mut array, true);
        monitor.record(&array, &stats(500, 50), 180);
        set_sleep(&mut array, false);
        monitor.record(&array, &stats(900, 50), 240);
        let open: BTreeMap<String, Vec<OpenFile>> = [(
            "disk2".to_string(),
            vec![OpenFile {
                path: "/mnt/user/Movies/x.mkv".into(),
                process: "Plex Media Serv".into(),
                container: Some("plex".into()),
            }],
        )]
        .into_iter()
        .collect();
        let summary = monitor.summary(&open);
        let disk2 = summary.iter().find(|a| a.disk == "disk2").unwrap();
        assert_eq!(disk2.wakeups, 2);
        assert_eq!(disk2.awake_minutes, 2);
        assert_eq!((disk2.reads, disk2.writes), (800, 40));
        assert_eq!(disk2.open.len(), 1);
        assert!(summary.iter().all(|a| a.disk != "flash"));
    }

    #[cfg(unix)]
    #[test]
    fn finds_who_holds_files_open() {
        let (_dir, system) = server();
        let array = array::read(&system);
        let file = system.file("/mnt/disk1/Movies/x.mkv");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"x").unwrap();
        let pid_dir = system.file("/proc/4242");
        std::fs::create_dir_all(pid_dir.join("fd")).unwrap();
        std::fs::write(pid_dir.join("comm"), "Plex Media Serv\n").unwrap();
        std::fs::write(
            pid_dir.join("cgroup"),
            "0::/docker/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\n",
        )
        .unwrap();
        std::os::unix::fs::symlink(system.file("/mnt/user/Movies/x.mkv"), pid_dir.join("fd/9"))
            .unwrap();
        let containers: BTreeMap<String, String> = [(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            "plex".to_string(),
        )]
        .into_iter()
        .collect();
        let open = open_by_disk(&system, &array, &containers);
        let disk1 = &open["disk1"];
        assert_eq!(disk1[0].path, "/mnt/user/Movies/x.mkv");
        assert_eq!(disk1[0].container.as_deref(), Some("plex"));
        assert!(!open.contains_key("disk2"));
        std::fs::write(
            system.file("/proc/diskstats"),
            "   8      32 sdc 100 0 2048 50 7 0 4096 9 0 0 0\n",
        )
        .unwrap();
        assert_eq!(diskstats(&system)["sdc"], (2048, 4096));
    }
}
