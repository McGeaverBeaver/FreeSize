//! Folder sizes of earlier scans, kept per scanned folder in FreeSize's data
//! folder (`scans/<hash>.json`), for the Changes tab and its chart.
//!
//! The newest scans keep their folders in full. Older ones keep only their
//! biggest folders, and get fewer as they age, like the Hub's history: every
//! scan of the last week, then one a day for three months and one a week
//! for the rest of the year. Scans over a year old are dropped.

use std::fs;
use std::path::{Path, PathBuf};

use freesize_core::snapshot::Snapshot;
use serde::{Deserialize, Serialize};

use crate::growth::data_dir;
use crate::history::write_atomically;

/// The newest scans, kept in full.
const KEEP: usize = 12;
/// Folders kept of an older scan: enough for its biggest changes.
const OLD_FOLDERS: usize = 300;
const DAY: i64 = 24 * 60 * 60;
/// Folders kept per scan.
pub const MAX_FOLDERS: usize = 10_000;

#[derive(Default, Serialize, Deserialize)]
struct Saved {
    root: String,
    #[serde(default)]
    snapshots: Vec<Snapshot>,
}

fn file_for(root: &Path) -> Option<PathBuf> {
    // FNV-1a of the path names the file.
    let hash = root
        .to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
        });
    Some(data_dir()?.join("scans").join(format!("{hash:016x}.json")))
}

/// Adds `snapshot` to `root`'s scans and returns the earlier ones, oldest
/// first. Problems only mean there is nothing to compare with.
pub fn record(root: &Path, snapshot: &Snapshot) -> Vec<Snapshot> {
    let Some(file) = file_for(root) else {
        return Vec::new();
    };
    let root_text = root.to_string_lossy().into_owned();
    let mut saved = fs::read(&file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
        .filter(|saved| saved.root == root_text)
        .unwrap_or_else(|| Saved {
            root: root_text,
            snapshots: Vec::new(),
        });
    let earlier = saved.snapshots.clone();
    saved.snapshots.push(snapshot.clone());
    saved.snapshots = thin(std::mem::take(&mut saved.snapshots), snapshot.taken);
    if let Ok(json) = serde_json::to_vec(&saved) {
        let _ = write_atomically(&file, &json);
    }
    earlier
}

/// Which snapshot age bucket `taken` falls in, `now` being the newest scan;
/// one snapshot per bucket is kept. `None` for over a year.
fn bucket(taken: i64, now: i64) -> Option<(u8, i64)> {
    let age = now - taken;
    if age < 7 * DAY {
        Some((0, taken))
    } else if age < 90 * DAY {
        Some((1, taken.div_euclid(DAY)))
    } else if age <= 365 * DAY {
        Some((2, taken.div_euclid(7 * DAY)))
    } else {
        None
    }
}

/// Thins `snapshots` (oldest first) as the module says.
fn thin(snapshots: Vec<Snapshot>, now: i64) -> Vec<Snapshot> {
    let count = snapshots.len();
    let mut kept: Vec<Snapshot> = Vec::new();
    for (i, snapshot) in snapshots.into_iter().enumerate() {
        let recent = i + KEEP >= count;
        let Some(here) = bucket(snapshot.taken, now) else {
            if recent {
                kept.push(snapshot);
            }
            continue;
        };
        // The latest of a bucket wins.
        if let Some(last) = kept.last()
            && !recent
            && bucket(last.taken, now) == Some(here)
        {
            kept.pop();
        }
        kept.push(if recent {
            snapshot
        } else {
            snapshot.trimmed(OLD_FOLDERS)
        });
    }
    kept
}

#[cfg(test)]
mod tests {
    use freesize_core::snapshot::FolderSize;

    use super::*;

    fn snap(taken: i64, folders: usize) -> Snapshot {
        Snapshot {
            taken,
            folders: (0..folders)
                .map(|n| FolderSize {
                    path: if n == 0 {
                        String::new()
                    } else {
                        format!("f{n}")
                    },
                    size: 1000 - n as u64,
                    allocated: 1000 - n as u64,
                })
                .collect(),
            floor: 0,
        }
    }

    #[test]
    fn keeps_a_year_thinned() {
        let now = 400 * DAY;
        // Two scans a day for 400 days.
        let all: Vec<Snapshot> = (0..800).map(|n| snap(n * DAY / 2 + 1, 500)).collect();
        let kept = thin(all, now);
        // The newest twelve in full, the rest trimmed.
        assert!(kept.iter().rev().take(KEEP).all(|s| s.folders.len() == 500));
        assert!(
            kept.iter()
                .rev()
                .skip(KEEP)
                .all(|s| s.folders.len() == OLD_FOLDERS && s.floor > 0)
        );
        // Nothing over a year old but the newest ones.
        assert!(kept.iter().all(|s| now - s.taken <= 365 * DAY));
        // A week of every scan (14), about 83 days of one a day, about 39
        // weeks of one a week.
        assert!((130..=145).contains(&kept.len()), "{}", kept.len());
        assert!(kept.windows(2).all(|w| w[0].taken < w[1].taken));
        // Thinning again keeps the same.
        assert_eq!(thin(kept.clone(), now).len(), kept.len());
    }
}
