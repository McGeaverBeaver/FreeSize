//! Folder sizes of earlier scans, kept per scanned folder in FreeSize's data
//! folder (`scans/<hash>.json`), for the Changes tab.

use std::fs;
use std::path::{Path, PathBuf};

use freesize_core::snapshot::Snapshot;
use serde::{Deserialize, Serialize};

use crate::growth::data_dir;
use crate::history::write_atomically;

/// Scans kept per folder.
const KEEP: usize = 12;
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
    if saved.snapshots.len() > KEEP {
        saved.snapshots.drain(..saved.snapshots.len() - KEEP);
    }
    if let Ok(json) = serde_json::to_vec(&saved) {
        let _ = write_atomically(&file, &json);
    }
    earlier
}
