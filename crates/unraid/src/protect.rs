//! What must never be deleted on an Unraid server, and where its
//! quarantine is.
//!
//! Never offered, whoever asks: the flash drive (`/boot`), the system
//! share (docker.img, libvirt.img), other machines' storage mounted here,
//! whole disks and whole shares, Docker's storage, the appdata of apps that
//! are installed, folders mounted into containers, the VMs' disks and the
//! ISOs in their CD drives, and the quarantine itself. Files open right now
//! are refused by the agent's own guard.

use std::path::{Path, PathBuf};

use disktree_agent::commands::Guard;
use disktree_agent::quarantine::{Place, Places, QUARANTINE_DIR, filesystem_root};

use crate::System;
use crate::array::{Array, mounts};
use crate::docker::{self, DockerConfig, Inventory};
use crate::vms::{self, Definition, VmConfig, user_path};

/// A path to keep, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule {
    /// A server path; inside a share, as `/mnt/user/...`.
    pub path: String,
    pub why: String,
    /// Everything inside it too, not just the path itself.
    pub all: bool,
    /// A disk or share as a whole: kept from deleting, but what's inside
    /// may be moved between disks.
    pub whole: bool,
}

fn rule(path: &str, why: &str, all: bool) -> Rule {
    Rule {
        path: user_path(path.trim_end_matches('/')),
        why: why.into(),
        all,
        whole: false,
    }
}

fn whole(path: &str, why: &str) -> Rule {
    Rule {
        whole: true,
        ..rule(path, why, false)
    }
}

/// The rules for this server, from its array, Docker and VMs.
pub fn rules(
    array: &Array,
    shares: &[String],
    docker: (&DockerConfig, Option<&Inventory>),
    vms: (&VmConfig, Option<&[Definition]>),
) -> Vec<Rule> {
    let mut rules = vec![
        rule("/boot", "the flash drive Unraid runs from", true),
        rule(
            "/mnt/user/system",
            "Unraid's system share (docker.img, libvirt.img)",
            true,
        ),
        rule(
            "/mnt/remotes",
            "another machine's storage, mounted here",
            true,
        ),
        rule("/mnt/addons", "storage mounted by a plugin", true),
        rule(
            "/mnt/rootshare",
            "another machine's storage, mounted here",
            true,
        ),
        whole("/mnt/user", "all the shares"),
        whole("/mnt/user0", "all the shares"),
    ];
    for disk in &array.disks {
        if let Some(mount) = &disk.path {
            rules.push(Rule {
                path: mount.clone(),
                why: format!("all of {}", disk.name),
                all: false,
                whole: true,
            });
        }
    }
    for share in shares {
        rules.push(whole(
            &format!("/mnt/user/{share}"),
            "a whole share; delete what's inside it instead",
        ));
    }
    for (path, why, all) in docker::protected_paths(docker.0, docker.1) {
        rules.push(rule(&path, &why, all));
    }
    for (path, why, all) in vms::protected_paths(vms.0, vms.1) {
        rules.push(rule(&path, &why, all));
    }
    rules
}

/// Why `path` (a server path) must stay, if it must.
pub fn check(rules: &[Rule], path: &str) -> Option<String> {
    let path = path.trim_end_matches('/');
    if path.split('/').any(|part| part == QUARANTINE_DIR) {
        return Some("DiskTree's quarantine; empty or put back from there".into());
    }
    let user = user_path(path);
    let candidates = [path, user.as_str()];
    let under = |child: &str, parent: &str| child.starts_with(&format!("{parent}/"));
    for rule in rules {
        for candidate in candidates {
            if candidate == rule.path || (rule.all && under(candidate, &rule.path)) {
                return Some(rule.why.clone());
            }
            if under(&rule.path, candidate) {
                return Some(format!("it holds {} ({})", rule.path, rule.why));
            }
        }
    }
    None
}

/// The agent's guard for an Unraid server.
pub struct UnraidGuard {
    pub system: System,
    pub rules: Vec<Rule>,
}

impl Guard for UnraidGuard {
    fn check(&self, path: &Path) -> Option<String> {
        let server = self
            .system
            .unraid_path(path)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        check(&self.rules, &server)
    }
}

/// Where things are on an Unraid server: a `/mnt/user` path is on every
/// disk holding it, and each disk (or ZFS dataset) has its own quarantine,
/// so moving aside never copies between disks.
pub struct UnraidPlaces {
    pub system: System,
    pub array: Array,
}

impl UnraidPlaces {
    /// The quarantine on the filesystem holding `real` (a path on `disk`'s
    /// mount `mount`): the disk's top, or a ZFS dataset's.
    fn quarantine_for(&self, real: &Path, mount: &str) -> Result<PathBuf, String> {
        let disk_top = self.system.file(mount);
        let top = filesystem_root(real)?;
        // Never above the disk itself.
        let top = if top.starts_with(&disk_top) {
            top
        } else {
            disk_top
        };
        Ok(top.join(QUARANTINE_DIR))
    }
}

impl Places for UnraidPlaces {
    fn places(&self, path: &Path) -> Result<Vec<Place>, String> {
        let server = self
            .system
            .unraid_path(path)
            .ok_or_else(|| format!("{}: not on this server", path.display()))?;
        let mut places = Vec::new();
        for (disk, physical) in self.array.physical(&server) {
            let real = self.system.file(&physical);
            if std::fs::symlink_metadata(&real).is_err() {
                continue;
            }
            let mount = self
                .array
                .disk(&disk)
                .and_then(|d| d.path.clone())
                .ok_or_else(|| format!("{disk} isn't mounted"))?;
            places.push(Place {
                quarantine: self.quarantine_for(&real, &mount)?,
                path: real,
            });
        }
        if places.is_empty() && !server.starts_with("/mnt/") {
            return Err(format!("{server}: not on a disk or pool"));
        }
        Ok(places)
    }

    fn shown(&self, path: &Path) -> String {
        self.system
            .unraid_path(path)
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }

    fn roots(&self) -> Vec<PathBuf> {
        // Awake disks only: looking at a sleeping one would wake it.
        let awake: Vec<String> = self
            .array
            .roots(false)
            .iter()
            .filter_map(|d| d.path.clone())
            .collect();
        let mut roots: Vec<PathBuf> = awake
            .iter()
            .map(|m| self.system.file(m).join(QUARANTINE_DIR))
            .collect();
        // ZFS datasets inside a pool have their own.
        for mount in mounts(&self.system) {
            if mount.fs == "zfs"
                && awake
                    .iter()
                    .any(|m| mount.path.starts_with(&format!("{m}/")))
            {
                roots.push(self.system.file(&mount.path).join(QUARANTINE_DIR));
            }
        }
        roots.sort();
        roots.dedup();
        roots
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::{self, tests::server};
    use disktree_agent::quarantine::Quarantine;

    fn setup_rules(system: &System) -> Vec<Rule> {
        let array = array::read(system);
        let docker_cfg = docker::config(system);
        let inventory = Inventory {
            containers: vec![docker::Container {
                name: "plex".into(),
                mounts: vec!["/mnt/cache/appdata/plex".into(), "/mnt/user/Movies".into()],
                ..docker::Container::default()
            }],
            ..Inventory::default()
        };
        let vm_cfg = vms::config(system);
        let defs = vec![Definition {
            name: "Win".into(),
            disks: vec!["/mnt/user/domains/Win/vdisk1.img".into()],
            isos: vec![],
        }];
        rules(
            &array,
            &["Movies".into(), "appdata".into()],
            (&docker_cfg, Some(&inventory)),
            (&vm_cfg, Some(&defs)),
        )
    }

    #[test]
    fn keeps_what_the_server_needs() {
        let (_dir, system) = server();
        let rules = setup_rules(&system);
        let kept = |p: &str| check(&rules, p);
        assert!(kept("/boot/config/go").unwrap().contains("flash"));
        assert!(kept("/mnt/disk1/system/docker/docker.img").is_some());
        assert!(kept("/mnt/disk1").is_some());
        assert!(kept("/mnt/user/Movies").unwrap().contains("whole share"));
        assert!(kept("/mnt/disk1/Movies").unwrap().contains("whole share"));
        assert_eq!(
            kept("/mnt/user/appdata/plex/Library/db").as_deref(),
            Some("the settings of plex")
        );
        assert!(kept("/mnt/cache/appdata/plex").is_some());
        // A folder holding something kept.
        assert!(kept("/mnt/cache/appdata").unwrap().contains("whole share"));
        assert!(
            kept("/mnt/disk1/domains/Win")
                .unwrap()
                .starts_with("it holds")
        );
        assert!(
            kept("/mnt/user/domains/Win/vdisk1.img")
                .unwrap()
                .contains("VM Win")
        );
        assert!(kept("/mnt/remotes/nas/x").is_some());
        assert!(kept("/mnt/disk1/.disktree-quarantine/b1").is_some());
        // What's inside a mounted share is fine; so are other apps' leftovers.
        assert_eq!(kept("/mnt/user/Movies/Old Film (1999)"), None);
        assert_eq!(kept("/mnt/disk1/Movies/Old Film (1999)/film.mkv"), None);
        assert_eq!(kept("/mnt/user/appdata/oldapp"), None);
        assert_eq!(kept("/mnt/user/domains/Win/old.img"), None);
    }

    #[test]
    fn quarantines_on_each_disk() {
        let (_dir, system) = server();
        for (path, bytes) in [
            ("/mnt/disk1/Movies/Old/a.mkv", 3000),
            ("/mnt/cache/Movies/Old/b.mkv", 1000),
        ] {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, vec![1u8; bytes]).unwrap();
        }
        let places = UnraidPlaces {
            system: system.clone(),
            array: array::read(&system),
        };
        let found = places.places(&system.file("/mnt/user/Movies/Old")).unwrap();
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].quarantine,
            system.file("/mnt/disk1").join(QUARANTINE_DIR)
        );
        assert_eq!(
            found[1].quarantine,
            system.file("/mnt/cache").join(QUARANTINE_DIR)
        );
        let roots = places.roots();
        // disk2 sleeps.
        assert_eq!(
            roots,
            [
                system.file("/mnt/cache").join(QUARANTINE_DIR),
                system.file("/mnt/disk1").join(QUARANTINE_DIR)
            ]
        );
        let quarantine = Quarantine::new(Box::new(places));
        let moved = quarantine.move_in(
            "b1",
            "Unraid web UI",
            "",
            &[system.file("/mnt/user/Movies/Old")],
            5,
        );
        assert!(moved.results[0].is_ok());
        assert!(!system.file("/mnt/disk1/Movies/Old").exists());
        assert!(!system.file("/mnt/cache/Movies/Old").exists());
        let list = quarantine.list();
        assert_eq!(list[0].items.len(), 2);
        assert_eq!(quarantine.restore("b1").unwrap(), "put back 2 items");
        assert!(system.file("/mnt/disk1/Movies/Old/a.mkv").exists());
    }
}
