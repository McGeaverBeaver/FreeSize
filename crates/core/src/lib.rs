//! Core of FreeSize: a parallel disk usage scanner, a compact tree of the
//! results, summaries (largest files, usage by file type) and a treemap
//! layout. Nothing here depends on a UI.
//!
//! ```no_run
//! use freesize_core::{ScanOptions, ScanProgress, SizeMode, SortKey, format_size, scan};
//!
//! let progress = ScanProgress::new();
//! let result = scan("/home", &ScanOptions::default(), &progress)?;
//! let tree = &result.tree;
//! for child in tree.children_sorted(tree.root(), SortKey::Size(SizeMode::Allocated)) {
//!     println!("{:>10}  {}", format_size(tree.node(child).allocated), tree.name(child));
//! }
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod access;
pub mod bubbles;
mod format;
pub mod junk;
mod kinds;
pub mod leftovers;
pub mod ncdu;
mod platform;
mod reclaim;
pub mod renamed;
pub mod review;
mod scan;
pub mod snapshot;
mod space;
mod stats;
mod tree;
pub mod treemap;

pub use format::{format_count, format_size};
pub use kinds::{Kind, kinds};
pub use reclaim::{
    DuplicateCandidates, DuplicateGroup, DuplicateProgress, Rebuildable, Safety, Spot, SpotRule,
    default_spot_rules, duplicate_candidates, find_duplicates, old_files, rebuildable,
    system_spots,
};
pub use scan::{ProgressSnapshot, ScanOptions, ScanProgress, ScanResult, scan};
pub use space::{DiskSpace, disk_space};
pub use stats::{
    Bin, ExtensionStat, age_bins, extension_stats, has_extension, largest_files,
    largest_files_where, size_bins,
};
pub use tree::{Descendants, NO_NODE, Node, NodeId, NodeKind, SizeMode, SortKey, Tree, flags};
