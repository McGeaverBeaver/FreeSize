//! A record of every app FreeSize has seen, so a removed app can be found
//! again later ("what was that camera app called?").
//!
//! The history is a small JSON file. On Android it's kept in two places:
//! `Documents/FreeSize/app-history.json` in shared storage, which survives
//! uninstalling FreeSize, and FreeSize's private storage, which works without
//! All files access. Both are read and merged, and both are written.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::apps::{AppReport, AppUsage, Breakdown};

const DAY: u64 = 24 * 60 * 60;
/// Daily size samples kept per app, about four months.
const APP_SAMPLES: usize = 120;
/// Daily samples of the whole storage, a year.
const STORAGE_SAMPLES: usize = 365;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Entry {
    pub package: String,
    pub label: String,
    pub installer: Option<String>,
    pub version: String,
    /// When the app was installed, if Android said (seconds since 1970).
    pub installed_at: Option<u64>,
    pub first_seen: u64,
    pub last_seen: u64,
    /// When FreeSize first noticed the app was gone.
    pub removed_at: Option<u64>,
    /// Its size when last seen.
    pub size: u64,
    /// `[day, bytes]` pairs, one per day FreeSize measured the app; days
    /// count from 1970. Oldest first.
    pub sizes: Vec<[u64; 2]>,
}

impl Entry {
    /// Enough of an [`AppUsage`] to classify where the app came from.
    pub fn as_app(&self) -> AppUsage {
        AppUsage {
            package: self.package.clone(),
            label: self.label.clone(),
            installer: self.installer.clone(),
            version: self.version.clone(),
            installed_at: self.installed_at,
            ..AppUsage::default()
        }
    }
}

/// How the storage divided up on one day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageSample {
    pub day: u64,
    pub total: u64,
    pub free: u64,
    pub apps: u64,
    pub files: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct History {
    #[serde(default)]
    pub apps: Vec<Entry>,
    /// Oldest first.
    #[serde(default)]
    pub storage: Vec<StorageSample>,
}

/// Adds today's sample, replacing an earlier one from the same day, and
/// drops the oldest beyond `keep`.
fn add_sample<T>(samples: &mut Vec<T>, sample: T, keep: usize, day_of: impl Fn(&T) -> u64) {
    if samples
        .last()
        .is_some_and(|last| day_of(last) == day_of(&sample))
    {
        samples.pop();
    }
    samples.push(sample);
    if samples.len() > keep {
        samples.drain(..samples.len() - keep);
    }
}

impl History {
    /// Records the apps in `report`, and marks apps that were installed
    /// before but aren't any more as removed. Preinstalled apps aren't
    /// tracked: they can't be removed, only disabled.
    pub fn record(&mut self, report: &AppReport) {
        let now = report.now;
        let mut index: HashMap<String, usize> = self
            .apps
            .iter()
            .enumerate()
            .map(|(i, entry)| (entry.package.clone(), i))
            .collect();
        for app in report.apps.iter().filter(|app| !app.system) {
            let i = *index.entry(app.package.clone()).or_insert_with(|| {
                self.apps.push(Entry {
                    package: app.package.clone(),
                    first_seen: now,
                    ..Entry::default()
                });
                self.apps.len() - 1
            });
            let entry = &mut self.apps[i];
            entry.label = app.label.clone();
            entry.installer = app.installer.clone();
            entry.version = app.version.clone();
            entry.installed_at = app.installed_at.or(entry.installed_at);
            entry.last_seen = now;
            entry.removed_at = None;
            entry.size = app.size();
            add_sample(
                &mut entry.sizes,
                [now / DAY, app.size()],
                APP_SAMPLES,
                |s| s[0],
            );
        }
        // Judge removal by the full package list, not by the measured apps:
        // an app whose stats couldn't be read is still installed.
        let installed: std::collections::HashSet<&str> =
            report.installed.iter().map(String::as_str).collect();
        for entry in &mut self.apps {
            if entry.removed_at.is_none() && !installed.contains(entry.package.as_str()) {
                entry.removed_at = Some(now);
            }
        }
    }

    /// Records how the storage divides up today.
    pub fn record_storage(&mut self, now: u64, breakdown: Breakdown) {
        let sample = StorageSample {
            day: now / DAY,
            total: breakdown.total,
            free: breakdown.free,
            apps: breakdown.apps,
            files: breakdown.files,
        };
        add_sample(&mut self.storage, sample, STORAGE_SAMPLES, |s| s.day);
    }

    /// Apps that are no longer installed, most recently removed first.
    pub fn removed(&self) -> Vec<Entry> {
        let mut removed: Vec<Entry> = self
            .apps
            .iter()
            .filter(|entry| entry.removed_at.is_some())
            .cloned()
            .collect();
        removed.sort_by_key(|entry| std::cmp::Reverse(entry.removed_at));
        removed
    }

    pub fn forget(&mut self, package: &str) {
        self.apps.retain(|entry| entry.package != package);
    }

    /// Combines two copies, keeping the most recently updated entry of each
    /// app, and the longer storage record.
    fn merge(mut self, other: History) -> History {
        if other.storage.len() > self.storage.len() {
            self.storage = other.storage;
        }
        for entry in other.apps {
            match self.apps.iter_mut().find(|e| e.package == entry.package) {
                Some(existing) if existing.last_seen >= entry.last_seen => {}
                Some(existing) => *existing = entry,
                None => self.apps.push(entry),
            }
        }
        self
    }
}

/// Reads and merges every copy that exists. Unreadable copies are skipped.
pub fn load(paths: &[PathBuf]) -> History {
    paths
        .iter()
        .filter_map(|path| fs::read(path).ok())
        .filter_map(|bytes| serde_json::from_slice::<History>(&bytes).ok())
        .fold(History::default(), History::merge)
}

/// Writes every copy it can. Fails only if none could be written.
pub fn save(history: &History, paths: &[PathBuf]) -> Result<(), String> {
    // Compact: with size samples the pretty form is several times bigger.
    let json = serde_json::to_vec(history).map_err(|e| e.to_string())?;
    let mut last_error = None;
    let mut saved = false;
    for path in paths {
        match write_atomically(path, &json) {
            Ok(()) => saved = true,
            Err(err) => last_error = Some(format!("{}: {err}", path.display())),
        }
    }
    match (saved, last_error) {
        (false, Some(err)) => Err(err),
        _ => Ok(()),
    }
}

/// Writes to a temporary file first, so a crash never leaves half a file.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, bytes)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(package: &str, system: bool) -> AppUsage {
        AppUsage {
            package: package.into(),
            label: package.to_uppercase(),
            installer: Some("com.android.vending".into()),
            code_bytes: 10,
            data_bytes: 5,
            system,
            ..AppUsage::default()
        }
    }

    fn report(now: u64, apps: Vec<AppUsage>) -> AppReport {
        AppReport {
            installed: apps.iter().map(|a| a.package.clone()).collect(),
            apps,
            now,
            ..AppReport::default()
        }
    }

    #[test]
    fn records_installs_and_removals() {
        let mut history = History::default();
        history.record(&report(100, vec![app("a", false), app("b", false)]));
        assert!(history.removed().is_empty());

        history.record(&report(200, vec![app("a", false)]));
        let removed = history.removed();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].package, "b");
        assert_eq!(removed[0].removed_at, Some(200));
        assert_eq!(removed[0].first_seen, 100);
        assert_eq!(removed[0].size, 15);

        // Reinstalled.
        history.record(&report(300, vec![app("a", false), app("b", false)]));
        assert!(history.removed().is_empty());
        assert_eq!(history.apps.len(), 2);
    }

    #[test]
    fn keeps_one_size_sample_per_day() {
        let mut history = History::default();
        let mut app = app("a", false);
        history.record(&report(DAY * 10, vec![app.clone()]));
        app.data_bytes = 50;
        history.record(&report(DAY * 10 + 3600, vec![app.clone()]));
        history.record(&report(DAY * 11, vec![app.clone()]));
        assert_eq!(history.apps[0].sizes, [[10, 60], [11, 60]]);
        for day in 12..(12 + APP_SAMPLES as u64) {
            history.record(&report(DAY * day, vec![app.clone()]));
        }
        assert_eq!(history.apps[0].sizes.len(), APP_SAMPLES);
        assert_eq!(history.apps[0].sizes[0][0], 12);
    }

    #[test]
    fn records_storage_per_day() {
        let mut history = History::default();
        let breakdown = |free| Breakdown {
            total: 100,
            free,
            apps: 50,
            cache: 0,
            files: 10,
            system: 40 - free,
            outside: 0,
        };
        history.record_storage(DAY * 5, breakdown(20));
        history.record_storage(DAY * 5 + 60, breakdown(15));
        history.record_storage(DAY * 6, breakdown(10));
        let free: Vec<_> = history.storage.iter().map(|s| (s.day, s.free)).collect();
        assert_eq!(free, [(5, 15), (6, 10)]);
    }

    #[test]
    fn skips_preinstalled_apps() {
        let mut history = History::default();
        history.record(&report(100, vec![app("sys", true)]));
        assert!(history.apps.is_empty());
    }

    #[test]
    fn an_unmeasured_app_is_not_removed() {
        let mut history = History::default();
        history.record(&report(100, vec![app("a", false)]));
        let mut later = report(200, vec![]);
        later.installed = vec!["a".into()];
        history.record(&later);
        assert!(history.removed().is_empty());
    }

    #[test]
    fn saves_loads_and_merges_copies() {
        let dir = std::env::temp_dir().join(format!("freesize-history-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let paths = vec![
            dir.join("one/app-history.json"),
            dir.join("two/app-history.json"),
        ];

        let mut old = History::default();
        old.record(&report(100, vec![app("a", false), app("b", false)]));
        save(&old, &paths[1..]).unwrap();
        let mut new = History::default();
        new.record(&report(200, vec![app("a", false)]));
        save(&new, &paths[..1]).unwrap();

        let merged = load(&paths);
        assert_eq!(merged.apps.len(), 2);
        let a = merged.apps.iter().find(|e| e.package == "a").unwrap();
        assert_eq!(a.last_seen, 200);

        let mut forgotten = merged.clone();
        forgotten.forget("b");
        save(&forgotten, &paths).unwrap();
        assert_eq!(load(&paths).apps.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unreadable_copies_are_skipped() {
        let dir = std::env::temp_dir().join(format!("freesize-bad-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("app-history.json");
        fs::write(&path, b"not json").unwrap();
        assert_eq!(load(&[path, dir.join("missing.json")]), History::default());
        let _ = fs::remove_dir_all(&dir);
    }
}
