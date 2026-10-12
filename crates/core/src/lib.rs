//! Core of DiskTree: a parallel disk usage scanner, a compact tree of the
//! results, summaries (largest files, usage by file type) and a treemap
//! layout. Nothing here depends on a UI.
//!
//! ```no_run
//! use disktree_core::{ScanOptions, ScanProgress, SizeMode, SortKey, format_size, scan};
//!
//! let progress = ScanProgress::new();
//! let result = scan("/home", &ScanOptions::default(), &progress)?;
//! let tree = &result.tree;
//! for child in tree.children_sorted(tree.root(), SortKey::Size(SizeMode::Allocated)) {
//!     println!("{:>10}  {}", format_size(tree.node(child).allocated), tree.name(child));
//! }
//! # Ok::<(), std::io::Error>(())
//! ```

pub mod bubbles;
mod format;
mod kinds;
mod platform;
mod reclaim;
mod scan;
pub mod snapshot;
mod space;
mod stats;
mod tree;
pub mod treemap;

pub use format::{format_count, format_size};
pub use kinds::{Kind, kinds};
pub use reclaim::{
    DuplicateCandidates, DuplicateGroup, DuplicateProgress, Rebuildable, duplicate_candidates,
    find_duplicates, old_files, rebuildable,
};
pub use scan::{ProgressSnapshot, ScanOptions, ScanProgress, ScanResult, scan};
pub use space::{DiskSpace, disk_space};
pub use stats::{
    Bin, ExtensionStat, age_bins, extension_stats, has_extension, largest_files,
    largest_files_where, size_bins,
};
pub use tree::{Descendants, NO_NODE, Node, NodeId, NodeKind, SizeMode, SortKey, Tree, flags};
