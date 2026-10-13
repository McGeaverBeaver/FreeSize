//! VMs: libvirt's definitions (`/etc/libvirt/qemu`, on libvirt.img, readable
//! while the VM service runs), their vdisks and CD drives, snapshots, and
//! what in the domains and isos shares no VM uses.
//!
//! When the definitions can't be read (the VM service is stopped), which VM
//! uses what is unknown, so everything there is protected.

use std::collections::BTreeSet;

use freesize_proto::unraid::{Finding, Safety, Vm, VmDisk, VmReport};

use crate::System;
use crate::array::Array;
use crate::docker::share_of;
use crate::ini::parse_cfg;
use crate::scans::Scans;

/// Files listed per share at most.
const MAX_FILES: usize = 5_000;

/// The VM settings from `/boot/config/domain.cfg`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmConfig {
    pub enabled: bool,
    /// libvirt.img.
    pub image: String,
    /// The shares for vdisks and ISOs.
    pub domains: String,
    pub isos: String,
}

pub fn config(system: &System) -> VmConfig {
    let cfg = system
        .read("/boot/config/domain.cfg")
        .map(|t| parse_cfg(&t))
        .unwrap_or_default();
    let get = |k: &str, default: &str| {
        cfg.get(k)
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| default.into())
    };
    VmConfig {
        enabled: get("SERVICE", "disable") == "enable",
        image: get("IMAGE_FILE", "/mnt/user/system/libvirt/libvirt.img"),
        domains: share_of(&get("DOMAINDIR", "/mnt/user/domains/"))
            .unwrap_or_else(|| "domains".into()),
        isos: share_of(&get("MEDIADIR", "/mnt/user/isos/")).unwrap_or_else(|| "isos".into()),
    }
}

/// A VM as libvirt defines it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Definition {
    pub name: String,
    pub disks: Vec<String>,
    pub isos: Vec<String>,
}

/// The VMs libvirt knows, or `None` when its settings can't be read.
pub fn definitions(system: &System) -> Option<Vec<Definition>> {
    let dir = system.file("/etc/libvirt/qemu");
    let entries = std::fs::read_dir(dir).ok()?;
    let mut list: Vec<Definition> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "xml"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|xml| parse_domain(&xml))
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Some(list)
}

fn parse_domain(xml: &str) -> Option<Definition> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let root = doc.root_element();
    if !root.has_tag_name("domain") {
        return None;
    }
    let name = root
        .children()
        .find(|n| n.has_tag_name("name"))?
        .text()?
        .trim()
        .to_string();
    let mut definition = Definition {
        name,
        ..Definition::default()
    };
    for disk in root.descendants().filter(|n| n.has_tag_name("disk")) {
        let Some(file) = disk
            .children()
            .find(|n| n.has_tag_name("source"))
            .and_then(|s| s.attribute("file"))
        else {
            continue;
        };
        match disk.attribute("device") {
            Some("cdrom") => definition.isos.push(file.to_string()),
            _ => definition.disks.push(file.to_string()),
        }
    }
    Some(definition)
}

/// Snapshots libvirt keeps for a VM.
fn snapshots(system: &System, name: &str) -> u32 {
    let xml = std::fs::read_dir(system.file(&format!("/etc/libvirt/qemu/snapshot/{name}")))
        .map(|entries| entries.flatten().count() as u32)
        .unwrap_or(0);
    let db = system
        .read(&format!("/etc/libvirt/qemu/snapshotdb/{name}/snapshots.db"))
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.as_object().map(|o| o.len() as u32))
        .unwrap_or(0);
    xml.max(db)
}

/// A path as `share/rest`, whichever disk or `/mnt/user` it was given on.
fn in_share(path: &str) -> Option<(String, String)> {
    let share = share_of(path)?;
    let rest = path
        .trim_start_matches('/')
        .splitn(4, '/')
        .nth(3)
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();
    Some((share, rest))
}

/// Paths nothing may delete because of VMs: libvirt.img, every VM's disks
/// and attached ISOs, or the whole domains and isos shares when the VMs
/// can't be read.
pub fn protected_paths(
    config: &VmConfig,
    definitions: Option<&[Definition]>,
) -> Vec<(String, String, bool)> {
    let mut list = vec![(
        config.image.clone(),
        "the VMs' settings (libvirt.img)".to_string(),
        true,
    )];
    match definitions {
        Some(vms) => {
            for vm in vms {
                for disk in &vm.disks {
                    list.push((
                        user_path(disk),
                        format!("a disk of the VM {}", vm.name),
                        true,
                    ));
                }
                for iso in &vm.isos {
                    list.push((
                        user_path(iso),
                        format!("in the CD drive of {}", vm.name),
                        true,
                    ));
                }
            }
        }
        None => {
            let why = "VM storage (the VM service is stopped, so which VM uses what can't be told)";
            list.push((format!("/mnt/user/{}", config.domains), why.into(), true));
            list.push((format!("/mnt/user/{}", config.isos), why.into(), true));
        }
    }
    list
}

/// A share path as `/mnt/user/...`, whichever disk it names.
pub fn user_path(path: &str) -> String {
    match in_share(path) {
        Some((share, rest)) if rest.is_empty() => format!("/mnt/user/{share}"),
        Some((share, rest)) => format!("/mnt/user/{share}/{rest}"),
        None => path.to_string(),
    }
}

pub fn report(system: &System, config: &VmConfig, array: &Array, scans: &Scans) -> VmReport {
    let definitions = definitions(system);
    let mut report = VmReport {
        enabled: config.enabled,
        ..VmReport::default()
    };
    let known = definitions.is_some();
    let defs = definitions.unwrap_or_default();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for def in &defs {
        let mut vm = Vm {
            name: def.name.clone(),
            running: system
                .file(&format!("/var/run/libvirt/qemu/{}.pid", def.name))
                .exists(),
            isos: def.isos.clone(),
            snapshots: snapshots(system, &def.name),
            disks: Vec::new(),
        };
        for disk in &def.disks {
            let sizes = scans.size(array, &user_path(disk));
            vm.disks.push(VmDisk {
                path: disk.clone(),
                provisioned: sizes.map(|s| s.apparent),
                used: sizes.map(|s| s.allocated),
            });
            used.insert(user_path(disk));
        }
        for iso in &def.isos {
            used.insert(user_path(iso));
        }
        report.vms.push(vm);
    }

    // ISOs.
    let isos_root = format!("/mnt/user/{}", config.isos);
    for (rest, sizes) in scans.files(array, &isos_root, MAX_FILES) {
        let path = format!("{isos_root}/{rest}");
        let user = defs
            .iter()
            .find(|d| d.isos.iter().any(|i| user_path(i) == path))
            .map(|d| d.name.clone());
        let (safety, reason) = match user {
            Some(vm) => (Safety::Protected, format!("in the CD drive of {vm}")),
            None if !known => (
                Safety::Protected,
                "the VM service is stopped, so which VM uses it can't be told".into(),
            ),
            None => (Safety::Check, "no VM has it in a CD drive".into()),
        };
        report.findings.push(Finding {
            id: format!("iso:{rest}"),
            kind: "iso".into(),
            label: rest,
            path: Some(path),
            bytes: sizes.allocated,
            safety,
            reason,
        });
    }

    // The domains share: folders no VM uses, and files in a VM's folder it
    // doesn't use (an old disk, or a snapshot's file).
    let domains_root = format!("/mnt/user/{}", config.domains);
    for (folder, entry) in scans.list(array, &domains_root) {
        let folder_path = format!("{domains_root}/{folder}");
        let owner = defs.iter().find(|d| {
            d.disks
                .iter()
                .any(|disk| user_path(disk).starts_with(&format!("{folder_path}/")))
        });
        if !known {
            report.findings.push(Finding {
                id: format!("vdisk:{folder}"),
                kind: "vdisk".into(),
                label: folder.clone(),
                path: Some(folder_path),
                bytes: entry.sizes.allocated,
                safety: Safety::Protected,
                reason: "the VM service is stopped, so which VM uses it can't be told".into(),
            });
            continue;
        }
        let Some(owner) = owner else {
            report.findings.push(Finding {
                id: format!("vdisk:{folder}"),
                kind: "vdisk".into(),
                label: folder.clone(),
                path: Some(folder_path),
                bytes: entry.sizes.allocated,
                safety: Safety::Check,
                reason: "no VM uses anything in it; a removed VM's disks?".into(),
            });
            continue;
        };
        for (rest, sizes) in scans.files(array, &folder_path, MAX_FILES) {
            let path = format!("{folder_path}/{rest}");
            let (kind, safety, reason) = if used.contains(&path) {
                (
                    "vdisk",
                    Safety::Protected,
                    format!("a disk of the VM {}", owner.name),
                )
            } else {
                (
                    "snapshot",
                    Safety::Check,
                    format!(
                        "in {}'s folder but not attached to it: an old disk or a snapshot's file",
                        owner.name
                    ),
                )
            };
            report.findings.push(Finding {
                id: format!("vdisk:{folder}/{rest}"),
                kind: kind.into(),
                label: format!("{folder}/{rest}"),
                path: Some(path),
                bytes: sizes.allocated,
                safety,
                reason,
            });
        }
    }
    report
        .findings
        .sort_by(|a, b| a.safety.cmp(&b.safety).then(b.bytes.cmp(&a.bytes)));
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::tests::server;
    use crate::scans::tests::scanned;

    const WIN11: &str = r#"<domain type='kvm'>
  <name>Windows 11</name>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='raw'/>
      <source file='/mnt/user/domains/Windows 11/vdisk1.img'/>
      <target dev='hdc' bus='virtio'/>
    </disk>
    <disk type='file' device='cdrom'>
      <source file='/mnt/user/isos/Win11_24H2.iso'/>
    </disk>
    <disk type='block' device='disk'>
      <source dev='/dev/disk/by-id/nvme-Samsung'/>
    </disk>
  </devices>
</domain>"#;

    fn setup() -> (tempfile::TempDir, System) {
        let (dir, system) = server();
        let qemu = system.file("/etc/libvirt/qemu");
        std::fs::create_dir_all(qemu.join("snapshot/Windows 11")).unwrap();
        std::fs::write(qemu.join("Windows 11.xml"), WIN11).unwrap();
        std::fs::write(
            qemu.join("snapshot/Windows 11/before-update.xml"),
            "<domainsnapshot/>",
        )
        .unwrap();
        std::fs::create_dir_all(system.file("/var/run/libvirt/qemu")).unwrap();
        std::fs::write(system.file("/var/run/libvirt/qemu/Windows 11.pid"), "123").unwrap();
        std::fs::create_dir_all(system.file("/boot/config")).unwrap();
        std::fs::write(
            system.file("/boot/config/domain.cfg"),
            "SERVICE=\"enable\"\nIMAGE_FILE=\"/mnt/user/system/libvirt/libvirt.img\"\n\
             DOMAINDIR=\"/mnt/user/domains/\"\nMEDIADIR=\"/mnt/user/isos/\"\n",
        )
        .unwrap();
        (dir, system)
    }

    #[test]
    fn reads_vms_and_sorts_their_files() {
        let (_dir, system) = setup();
        let (array, scans) = scanned(
            &system,
            &[
                ("/mnt/cache/domains/Windows 11/vdisk1.img", 60_000),
                ("/mnt/cache/domains/Windows 11/vdisk2.img", 20_000),
                ("/mnt/cache/domains/Old Ubuntu/vdisk1.img", 40_000),
                ("/mnt/disk1/isos/Win11_24H2.iso", 50_000),
                ("/mnt/disk1/isos/ubuntu-22.04.iso", 30_000),
            ],
        );
        let cfg = config(&system);
        assert!(cfg.enabled);
        let report = report(&system, &cfg, &array, &scans);
        assert_eq!(report.vms.len(), 1);
        let vm = &report.vms[0];
        assert_eq!(vm.name, "Windows 11");
        assert!(vm.running);
        assert_eq!(vm.snapshots, 1);
        assert_eq!(vm.isos, ["/mnt/user/isos/Win11_24H2.iso"]);
        assert_eq!(vm.disks.len(), 1);
        assert_eq!(vm.disks[0].provisioned, Some(60_000));
        let find = |id: &str| {
            report
                .findings
                .iter()
                .find(|f| f.id == id)
                .unwrap_or_else(|| panic!("{id}"))
        };
        assert_eq!(find("iso:Win11_24H2.iso").safety, Safety::Protected);
        assert_eq!(find("iso:ubuntu-22.04.iso").safety, Safety::Check);
        assert_eq!(
            find("vdisk:Windows 11/vdisk1.img").safety,
            Safety::Protected
        );
        assert_eq!(find("vdisk:Windows 11/vdisk2.img").kind, "snapshot");
        assert_eq!(find("vdisk:Old Ubuntu").safety, Safety::Check);

        let defs = definitions(&system).unwrap();
        let protected = protected_paths(&cfg, Some(&defs));
        assert!(
            protected
                .iter()
                .any(|(p, _, _)| p == "/mnt/user/domains/Windows 11/vdisk1.img")
        );
        assert!(
            protected
                .iter()
                .any(|(p, _, _)| p == "/mnt/user/system/libvirt/libvirt.img")
        );
    }

    #[test]
    fn stopped_vm_service_protects_everything() {
        let (_dir, system) = setup();
        std::fs::remove_dir_all(system.file("/etc/libvirt")).unwrap();
        let (array, scans) = scanned(&system, &[("/mnt/disk1/isos/a.iso", 1_000)]);
        let cfg = config(&system);
        let report = report(&system, &cfg, &array, &scans);
        assert!(
            report
                .findings
                .iter()
                .all(|f| f.safety == Safety::Protected)
        );
        assert_eq!(definitions(&system), None);
        let protected = protected_paths(&cfg, None);
        assert!(
            protected
                .iter()
                .any(|(p, _, all)| p == "/mnt/user/isos" && *all)
        );
        assert_eq!(
            user_path("/mnt/cache/domains/x/v.img"),
            "/mnt/user/domains/x/v.img"
        );
    }
}
