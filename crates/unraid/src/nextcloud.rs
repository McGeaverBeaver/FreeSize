//! Nextcloud's own data folder on the server: how much of it is the
//! people's files, and how much previews, old versions, the trash and
//! uploads in progress, per user, with the retention settings that would
//! have cleared them. Only reported: Nextcloud's database lists every one
//! of these files, so they're removed through Nextcloud (`occ
//! versions:cleanup`, `occ trashbin:cleanup`, the settings), never from
//! the file system.

use freesize_proto::unraid::{NextcloudReport, NextcloudUser};

use crate::System;
use crate::array::Array;
use crate::scans::{Keep, Scans};

/// How deep below a disk's top the data folder is looked for: a share
/// (`/mnt/user/nextcloud`), a folder in appdata (`appdata/nextcloud/data`)
/// or one deeper (`appdata/nextcloud/www/data`).
const SEARCH_DEPTH: usize = 3;

/// One Nextcloud install found on the server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Instance {
    /// The data folder, as a `/mnt/user` path.
    pub data: String,
    /// `config.php`, as a `/mnt/user` path, when it's beside the data
    /// folder or in the appdata folder with it.
    pub config: Option<String>,
}

/// Finds data folders by the `.ncdata` (or older `.ocdata`) file every
/// one holds, on each disk down to a few levels, without waking anything:
/// sleeping disks aren't mounted for reading here.
pub fn find(system: &System, array: &Array) -> Vec<Instance> {
    let mut found: Vec<Instance> = Vec::new();
    for disk in array.roots(false) {
        let Some(mount) = &disk.path else { continue };
        let top = system.file(mount);
        let Ok(shares) = std::fs::read_dir(&top) else {
            continue;
        };
        let mut stack: Vec<(std::path::PathBuf, usize)> = shares
            .flatten()
            .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
            .map(|e| (e.path(), 1))
            .collect();
        while let Some((dir, depth)) = stack.pop() {
            if !dir.is_dir() {
                continue;
            }
            if dir.join(".ncdata").is_file() || dir.join(".ocdata").is_file() {
                let relative = dir.strip_prefix(&top).unwrap_or(&dir).to_string_lossy();
                let data = format!("/mnt/user/{relative}");
                if found.iter().any(|i| i.data == data) {
                    continue;
                }
                let config = config_near(system, array, &data);
                found.push(Instance { data, config });
                continue;
            }
            if depth < SEARCH_DEPTH
                && let Ok(entries) = std::fs::read_dir(&dir)
            {
                // Not into the people's files: a data folder is never
                // inside another one, and `files` folders are big.
                stack.extend(
                    entries
                        .flatten()
                        .filter(|e| {
                            let name = e.file_name().to_string_lossy().into_owned();
                            !name.starts_with('.')
                                && name != "files"
                                && !name.starts_with("appdata_")
                        })
                        .map(|e| (e.path(), depth + 1)),
                );
            }
        }
    }
    found.sort_by(|a, b| a.data.cmp(&b.data));
    found
}

/// `config/config.php` beside the data folder (the official image keeps
/// `config` and `data` together), else `config/config.php` or
/// `www/nextcloud/config/config.php` in the same appdata folder.
fn config_near(system: &System, array: &Array, data: &str) -> Option<String> {
    let parent = data.rsplit_once('/').map(|(p, _)| p)?;
    let candidates = [
        format!("{parent}/config/config.php"),
        format!("{parent}/www/nextcloud/config/config.php"),
        format!("{parent}/nextcloud/config/config.php"),
    ];
    candidates.into_iter().find(|candidate| {
        array
            .physical(candidate)
            .iter()
            .any(|(_, physical)| system.file(physical).is_file())
    })
}

/// The files the scans must keep for the report: versions and trash per
/// user, and the group folders', to tell how old they are.
pub fn keep(instances: &[Instance]) -> Keep {
    let mut keep = Keep::default();
    for instance in instances {
        for place in [
            "*/files_versions",
            "*/files_trashbin",
            "__groupfolders/versions",
            "__groupfolders/trash",
        ] {
            keep.under.push(format!("{}/{place}", instance.data));
        }
    }
    keep
}

/// What a `config.php` says, for the few settings that matter here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub version: String,
    pub versions: String,
    pub trash: String,
    pub previews: bool,
    pub maintenance: bool,
}

impl Default for Config {
    /// Nextcloud's own defaults.
    fn default() -> Config {
        Config {
            version: String::new(),
            versions: "auto".into(),
            trash: "auto".into(),
            previews: true,
            maintenance: false,
        }
    }
}

/// Reads `'key' => value,` lines; enough for Nextcloud's config, which is
/// one PHP array written by Nextcloud itself.
pub fn parse_config(text: &str) -> Config {
    let mut config = Config::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once("=>") else {
            continue;
        };
        let key = key.trim().trim_matches(|c| c == '\'' || c == '"');
        let value = value
            .trim()
            .trim_end_matches(',')
            .trim()
            .trim_matches(|c| c == '\'' || c == '"')
            .to_string();
        match key {
            "version" => config.version = value,
            "versions_retention_obligation" => config.versions = value,
            "trashbin_retention_obligation" => config.trash = value,
            "enable_previews" => config.previews = value != "false",
            "maintenance" => config.maintenance = value == "true",
            _ => {}
        }
    }
    config
}

/// The number of days after which a retention setting removes things for
/// sure: `auto, 30` and `7, 30` say 30; `auto`, `30, auto` and `disabled`
/// set no such day.
pub fn latest_days(setting: &str) -> Option<u32> {
    let parts: Vec<&str> = setting.split(',').map(str::trim).collect();
    match parts.as_slice() {
        [_, days] => days.parse().ok(),
        _ => None,
    }
}

/// What a retention setting means, in words.
pub fn describe(setting: &str, what: &str) -> String {
    let parts: Vec<&str> = setting.split(',').map(str::trim).collect();
    match parts.as_slice() {
        ["disabled"] => format!("{what} are never removed by Nextcloud"),
        ["auto"] => format!("{what} are kept until space runs low (auto)"),
        [days, "auto"] => {
            format!("{what} are kept at least {days} days, longer while there's room")
        }
        ["auto", days] => {
            format!("{what} are removed when space runs low, and after {days} days at the latest")
        }
        [least, most] => format!("{what} are kept {least} to {most} days"),
        _ => format!("{what}: {setting}"),
    }
}

/// Folders in the data folder that aren't a person's.
fn is_system(name: &str) -> bool {
    name.starts_with("appdata_")
        || name.starts_with("updater-")
        || name.starts_with('.')
        || matches!(name, "__groupfolders" | "files_external" | "nextcloud.log")
}

fn allocated(scans: &Scans, array: &Array, path: &str) -> u64 {
    scans.size(array, path).map_or(0, |s| s.allocated)
}

/// The report for one install, from this round's scans.
pub fn report(
    system: &System,
    array: &Array,
    scans: &Scans,
    instance: &Instance,
    now: i64,
) -> Option<NextcloudReport> {
    let data = &instance.data;
    let total = scans.size(array, data)?.allocated;
    let config = instance
        .config
        .as_ref()
        .and_then(|path| {
            array
                .physical(path)
                .iter()
                .find_map(|(_, physical)| std::fs::read_to_string(system.file(physical)).ok())
        })
        .map(|text| parse_config(&text))
        .unwrap_or_default();
    let mut report = NextcloudReport {
        data: data.clone(),
        config: instance.config.clone().unwrap_or_default(),
        version: config.version.clone(),
        total,
        versions_setting: config.versions.clone(),
        trash_setting: config.trash.clone(),
        ..NextcloudReport::default()
    };
    let day = 86_400;
    let versions_cutoff = latest_days(&config.versions).map(|d| now - i64::from(d) * day);
    let trash_cutoff = latest_days(&config.trash).map(|d| now - i64::from(d) * day);
    for (name, entry) in scans.list(array, data) {
        if !entry.dir {
            continue;
        }
        let folder = format!("{data}/{name}");
        if name.starts_with("appdata_") {
            let previews = allocated(scans, array, &format!("{folder}/preview"));
            report.previews += previews;
            report.appdata += entry.sizes.allocated.saturating_sub(previews);
            continue;
        }
        if name == "__groupfolders" {
            let versions = format!("{folder}/versions");
            let trash = format!("{folder}/trash");
            let mut user = NextcloudUser {
                name: "Group folders".into(),
                versions: allocated(scans, array, &versions),
                trash: allocated(scans, array, &trash),
                ..NextcloudUser::default()
            };
            user.files = entry
                .sizes
                .allocated
                .saturating_sub(user.versions + user.trash);
            if let Some(cutoff) = versions_cutoff {
                report.versions_expired += scans.older_than(array, &versions, cutoff).1.allocated;
            }
            if let Some(cutoff) = trash_cutoff {
                report.trash_expired += scans.older_than(array, &trash, cutoff).1.allocated;
            }
            report.users.push(user);
            continue;
        }
        if is_system(&name) || scans.size(array, &format!("{folder}/files")).is_none() {
            continue;
        }
        let versions = format!("{folder}/files_versions");
        let trash = format!("{folder}/files_trashbin");
        let user = NextcloudUser {
            name: name.clone(),
            files: allocated(scans, array, &format!("{folder}/files")),
            versions: allocated(scans, array, &versions),
            trash: allocated(scans, array, &trash),
            uploads: allocated(scans, array, &format!("{folder}/uploads")),
            cache: allocated(scans, array, &format!("{folder}/cache")),
        };
        if let Some(cutoff) = versions_cutoff {
            report.versions_expired += scans.older_than(array, &versions, cutoff).1.allocated;
        }
        if let Some(cutoff) = trash_cutoff {
            report.trash_expired += scans.older_than(array, &trash, cutoff).1.allocated;
        }
        report.uploads += user.uploads;
        report.users.push(user);
    }
    report.users.sort_by(|a, b| {
        (b.versions + b.trash)
            .cmp(&(a.versions + a.trash))
            .then(a.name.cmp(&b.name))
    });

    // What to know.
    report
        .notes
        .push(describe(&config.versions, "Old versions"));
    report
        .notes
        .push(describe(&config.trash, "Files in the trash"));
    if report.versions_expired + report.trash_expired > 0 {
        report.notes.push(format!(
            "{} of versions and {} of trash are older than the settings allow. Nextcloud's \
             background job removes them; if these numbers don't fall, check in Administration \
             settings → Basic settings that the job runs (cron is the one that always does).",
            freesize_core::format_size(report.versions_expired),
            freesize_core::format_size(report.trash_expired)
        ));
    }
    if !config.previews && report.previews > 0 {
        report.notes.push(
            "Previews are turned off (enable_previews), so the preview folder only shrinks when \
             it's cleared."
                .into(),
        );
    }
    if config.maintenance {
        report
            .notes
            .push("Nextcloud is in maintenance mode: its background job isn't running.".into());
    }
    if instance.config.is_none() {
        report.notes.push(
            "config.php wasn't found near the data folder, so the retention settings are \
             Nextcloud's defaults here."
                .into(),
        );
    }
    report.notes.push(
        "Nothing here is offered for deletion: Nextcloud's database lists every one of these \
         files, so they're removed through Nextcloud itself (occ versions:cleanup, occ \
         trashbin:cleanup, the retention settings), never from the file system."
            .into(),
    );
    Some(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::{self, tests::server};
    use crate::scans::{DiskScan, Scans};

    #[test]
    fn retention_settings() {
        assert_eq!(latest_days("auto"), None);
        assert_eq!(latest_days("auto, 30"), Some(30));
        assert_eq!(latest_days("7, 30"), Some(30));
        assert_eq!(latest_days("30, auto"), None);
        assert_eq!(latest_days("disabled"), None);
        assert_eq!(
            describe("auto, 30", "Files in the trash"),
            "Files in the trash are removed when space runs low, and after 30 days at the latest"
        );
        let config = parse_config(
            "<?php\n$CONFIG = array (\n  'instanceid' => 'oc1',\n  'version' => '30.0.2.2',\n  \
             'trashbin_retention_obligation' => 'auto, 30',\n  'enable_previews' => false,\n  \
             'maintenance' => true,\n);\n",
        );
        assert_eq!(config.version, "30.0.2.2");
        assert_eq!(config.versions, "auto");
        assert_eq!(config.trash, "auto, 30");
        assert!(!config.previews && config.maintenance);
    }

    #[test]
    fn finds_the_data_folder_and_sizes_it_per_user() {
        let (_dir, system) = server();
        let write = |path: &str, bytes: usize, age_days: i64| {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, vec![1u8; bytes]).unwrap();
            let when = std::time::SystemTime::now()
                - std::time::Duration::from_secs(age_days as u64 * 86_400);
            std::fs::File::open(&file)
                .unwrap()
                .set_times(std::fs::FileTimes::new().set_modified(when))
                .unwrap();
        };
        write("/mnt/disk1/nextcloud/.ncdata", 1, 0);
        write("/mnt/disk1/nextcloud/bob/files/photo.jpg", 5_000, 0);
        write(
            "/mnt/disk1/nextcloud/bob/files_versions/photo.jpg.v1",
            1_000,
            60,
        );
        write(
            "/mnt/disk1/nextcloud/bob/files_versions/photo.jpg.v2",
            1_000,
            1,
        );
        write(
            "/mnt/cache/nextcloud/bob/files_trashbin/files/old.pdf.d1",
            3_000,
            45,
        );
        write("/mnt/disk1/nextcloud/bob/uploads/part", 500, 0);
        write("/mnt/disk1/nextcloud/appdata_oc1/preview/1/a.jpg", 2_000, 0);
        write("/mnt/disk1/nextcloud/appdata_oc1/theming/x", 100, 0);
        write("/mnt/disk1/nextcloud/updater-oc1/backups/x", 100, 0);
        write("/mnt/cache/appdata/nextcloud/config/config.php", 0, 0);
        std::fs::write(
            system.file("/mnt/cache/appdata/nextcloud/config/config.php"),
            "<?php\n$CONFIG = array (\n  'version' => '30.0.2.2',\n  'versions_retention_obligation' => 'auto, 30',\n  'trashbin_retention_obligation' => 'auto, 30',\n);\n",
        )
        .unwrap();
        // A second install in appdata, the official image's layout.
        write("/mnt/cache/appdata/nc2/data/.ncdata", 1, 0);
        write("/mnt/cache/appdata/nc2/data/ann/files/a", 10, 0);
        write("/mnt/cache/appdata/nc2/config/config.php", 0, 0);

        let array = array::read(&system);
        let instances = find(&system, &array);
        assert_eq!(
            instances,
            [
                Instance {
                    data: "/mnt/user/appdata/nc2/data".into(),
                    config: Some("/mnt/user/appdata/nc2/config/config.php".into()),
                },
                Instance {
                    data: "/mnt/user/nextcloud".into(),
                    config: None,
                },
            ]
        );
        let keep = keep(&instances);
        assert!(
            keep.under
                .contains(&"/mnt/user/nextcloud/*/files_versions".to_string())
        );
        let mut scans = Scans::default();
        for disk in array.roots(false) {
            let mount = disk.path.clone().unwrap();
            let tree = freesize_core::scan(
                system.file(&mount),
                &freesize_core::ScanOptions::default(),
                &freesize_core::ScanProgress::new(),
            )
            .unwrap()
            .tree;
            scans
                .disks
                .push(DiskScan::compact(&disk.name, &mount, &tree, 0, &keep));
        }
        let now = crate::now();
        let report = report(&system, &array, &scans, &instances[1], now).unwrap();
        assert_eq!(report.users.len(), 1);
        let bob = &report.users[0];
        assert_eq!(bob.name, "bob");
        assert!(bob.files >= 5_000 && bob.versions >= 2_000 && bob.trash >= 3_000);
        assert!(bob.uploads >= 500);
        assert!(report.previews >= 2_000);
        assert!(report.appdata >= 100 && report.appdata < report.previews + 2_000);
        // Without a config.php the defaults hold: nothing counts as expired.
        assert_eq!(report.versions_setting, "auto");
        assert_eq!(report.versions_expired, 0);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("config.php wasn't found"))
        );

        // With the 30-day settings, the 60-day version and the 45-day trash
        // are past them.
        let with_config = Instance {
            data: "/mnt/user/nextcloud".into(),
            config: Some("/mnt/user/appdata/nextcloud/config/config.php".into()),
        };
        let report = super::report(&system, &array, &scans, &with_config, now).unwrap();
        assert_eq!(report.version, "30.0.2.2");
        let bob = &report.users[0];
        assert!(report.versions_expired >= 1_000 && report.versions_expired < bob.versions);
        assert!(report.trash_expired >= 3_000);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("older than the settings allow"))
        );
    }
}
