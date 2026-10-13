//! Shares and which disks their files sit on. A share is a top-level folder
//! on any disk or pool; `shares.ini` adds its settings. Sizes come from the
//! disks' scans: this time's for disks that were awake, the last ones for
//! disks that slept.

use std::collections::{BTreeMap, BTreeSet};

use freesize_core::{SizeMode, Tree};
use freesize_proto::unraid::{Placement, ShareUse};
use serde::{Deserialize, Serialize};

use crate::System;
use crate::ini::parse_ini;

/// A share's settings.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShareConfig {
    pub name: String,
    pub comment: String,
    /// How it uses pools, in words: "array", "cache only", "cache → array", …
    pub cache: String,
}

/// The shares Unraid has settings for.
pub fn read(system: &System) -> Vec<ShareConfig> {
    let sections = system
        .read("/var/local/emhttp/shares.ini")
        .map(|t| parse_ini(&t))
        .unwrap_or_default();
    sections
        .into_iter()
        .map(|(section, map)| {
            let get = |k: &str| map.get(k).cloned().unwrap_or_default();
            let pool = map
                .get("cachePool")
                .filter(|p| !p.is_empty())
                .cloned()
                .unwrap_or_else(|| "cache".into());
            let secondary = map.get("cachePool2").filter(|p| !p.is_empty()).cloned();
            let cache = match get("useCache").as_str() {
                "no" => secondary.clone().unwrap_or_else(|| "array".into()),
                "only" => format!("{pool} only"),
                "yes" => format!("{pool} → {}", secondary.as_deref().unwrap_or("array")),
                "prefer" => format!("{} → {pool}", secondary.as_deref().unwrap_or("array")),
                _ => String::new(),
            };
            ShareConfig {
                name: map.get("name").cloned().unwrap_or(section),
                comment: get("comment"),
                cache,
            }
        })
        .collect()
}

/// The sizes of a disk's top-level folders (its shares), from one scan.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskTops {
    pub disk: String,
    /// When the scan was made.
    pub scanned: i64,
    pub tops: BTreeMap<String, u64>,
}

/// Top-level folder sizes on disk, from a scan of a disk's mount.
pub fn tops_of(tree: &Tree) -> BTreeMap<String, u64> {
    let root = tree.root();
    tree.children(root)
        .filter(|&id| tree.node(id).is_dir())
        .filter_map(|id| {
            let name = tree.name(id).into_owned();
            // FreeSize's quarantine and other hidden folders aren't shares.
            (!name.starts_with('.')).then(|| (name, tree.node(id).size_in(SizeMode::Allocated)))
        })
        .collect()
}

/// Every share and where its files are. `fresh` are this time's disk
/// scans, `stale` earlier ones of disks that slept this time.
pub fn distribution(
    configs: &[ShareConfig],
    fresh: &[DiskTops],
    stale: &[DiskTops],
) -> Vec<ShareUse> {
    let mut names: BTreeSet<String> = configs.iter().map(|c| c.name.clone()).collect();
    for tops in fresh.iter().chain(stale) {
        names.extend(tops.tops.keys().cloned());
    }
    let mut shares: Vec<ShareUse> = names
        .into_iter()
        .map(|name| {
            let config = configs.iter().find(|c| c.name == name);
            let mut placements = Vec::new();
            for (tops, is_stale) in fresh
                .iter()
                .map(|t| (t, false))
                .chain(stale.iter().map(|t| (t, true)))
            {
                if let Some(&bytes) = tops.tops.get(&name) {
                    placements.push(Placement {
                        disk: tops.disk.clone(),
                        bytes,
                        stale: is_stale,
                    });
                }
            }
            placements.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.disk.cmp(&b.disk)));
            ShareUse {
                total: placements.iter().map(|p| p.bytes).sum(),
                complete: !placements.iter().any(|p| p.stale),
                placements,
                comment: config.map(|c| c.comment.clone()).unwrap_or_default(),
                cache: config.map(|c| c.cache.clone()).unwrap_or_default(),
                name,
            }
        })
        .collect();
    shares.sort_by(|a, b| b.total.cmp(&a.total).then(a.name.cmp(&b.name)));
    shares
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::tests::server;

    #[test]
    fn reads_share_settings() {
        let (_dir, system) = server();
        std::fs::write(
            system.file("/var/local/emhttp/shares.ini"),
            "[\"appdata\"]\nname=\"appdata\"\ncomment=\"\"\nuseCache=\"prefer\"\ncachePool=\"cache\"\n\
             [\"Movies\"]\nname=\"Movies\"\ncomment=\"films\"\nuseCache=\"yes\"\ncachePool=\"cache\"\n\
             [\"isos\"]\nname=\"isos\"\nuseCache=\"no\"\n\
             [\"system\"]\nname=\"system\"\nuseCache=\"only\"\ncachePool=\"nvme\"\n",
        )
        .unwrap();
        let shares = read(&system);
        let cache: Vec<(&str, &str)> = shares
            .iter()
            .map(|s| (s.name.as_str(), s.cache.as_str()))
            .collect();
        assert_eq!(
            cache,
            [
                ("appdata", "array → cache"),
                ("Movies", "cache → array"),
                ("isos", "array"),
                ("system", "nvme only"),
            ]
        );
        assert_eq!(shares[1].comment, "films");
    }

    #[test]
    fn shares_spread_over_disks() {
        let tops = |disk: &str, list: &[(&str, u64)]| DiskTops {
            disk: disk.into(),
            scanned: 100,
            tops: list.iter().map(|(n, b)| (n.to_string(), *b)).collect(),
        };
        let configs = vec![
            ShareConfig {
                name: "Movies".into(),
                cache: "cache → array".into(),
                ..ShareConfig::default()
            },
            ShareConfig {
                name: "Empty".into(),
                ..ShareConfig::default()
            },
        ];
        let fresh = [
            tops("disk1", &[("Movies", 500), ("TV", 300)]),
            tops("cache", &[("Movies", 20), ("appdata", 50)]),
        ];
        let stale = [tops("disk2", &[("Movies", 700)])];
        let shares = distribution(&configs, &fresh, &stale);
        let names: Vec<&str> = shares.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Movies", "TV", "appdata", "Empty"]);
        let movies = &shares[0];
        assert_eq!(movies.total, 1220);
        assert!(!movies.complete);
        assert_eq!(movies.placements[0].disk, "disk2");
        assert!(movies.placements[0].stale);
        assert_eq!(movies.cache, "cache → array");
        assert!(shares[1].complete);
        assert_eq!(shares[3].total, 0);
    }

    #[test]
    fn tops_come_from_a_scan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("Movies/2024")).unwrap();
        std::fs::write(dir.path().join("Movies/2024/a.mkv"), vec![1u8; 100_000]).unwrap();
        std::fs::create_dir_all(dir.path().join(".freesize")).unwrap();
        std::fs::write(dir.path().join("loose.txt"), b"x").unwrap();
        let tree = freesize_core::scan(
            dir.path(),
            &freesize_core::ScanOptions::default(),
            &freesize_core::ScanProgress::new(),
        )
        .unwrap()
        .tree;
        let tops = tops_of(&tree);
        assert_eq!(tops.keys().collect::<Vec<_>>(), ["Movies"]);
        assert!(tops["Movies"] >= 100_000);
    }
}
