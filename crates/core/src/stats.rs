//! Summaries computed from a scanned tree.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::tree::{NodeId, NodeKind, SizeMode, Tree, flags};

/// The `n` largest files below `under`, largest first.
pub fn largest_files(tree: &Tree, under: NodeId, mode: SizeMode, n: usize) -> Vec<NodeId> {
    if n == 0 {
        return Vec::new();
    }
    // Min-heap holding the best `n` seen so far.
    let mut heap: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::with_capacity(n + 1);
    for id in tree.descendants(under) {
        let node = tree.node(id);
        if node.kind == NodeKind::Dir || node.has(flags::HARDLINK_DUP) {
            continue;
        }
        let size = node.size_in(mode);
        if heap.len() < n {
            heap.push(Reverse((size, id)));
        } else if heap.peek().is_some_and(|Reverse((min, _))| size > *min) {
            heap.pop();
            heap.push(Reverse((size, id)));
        }
    }
    let mut out: Vec<(u64, NodeId)> = heap.into_iter().map(|Reverse(e)| e).collect();
    out.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    out.into_iter().map(|(_, id)| id).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionStat {
    /// Lower-case extension without the dot; empty for files without one.
    pub extension: String,
    pub size: u64,
    pub files: u64,
}

/// Total size and count of files per extension below `under`, largest first.
pub fn extension_stats(tree: &Tree, under: NodeId, mode: SizeMode) -> Vec<ExtensionStat> {
    let mut totals: HashMap<Vec<u8>, (u64, u64)> = HashMap::new();
    let mut key = Vec::new();
    for id in tree.descendants(under) {
        let node = tree.node(id);
        if node.kind == NodeKind::Dir || node.has(flags::HARDLINK_DUP) {
            continue;
        }
        key.clear();
        if let Some(ext) = tree.extension(id) {
            key.extend(ext.iter().map(u8::to_ascii_lowercase));
        }
        let size = node.size_in(mode);
        if let Some(total) = totals.get_mut(key.as_slice()) {
            total.0 += size;
            total.1 += 1;
        } else {
            totals.insert(key.clone(), (size, 1));
        }
    }
    let mut out: Vec<ExtensionStat> = totals
        .into_iter()
        .map(|(ext, (size, files))| ExtensionStat {
            extension: String::from_utf8_lossy(&ext).into_owned(),
            size,
            files,
        })
        .collect();
    out.sort_by(|a, b| {
        b.size
            .cmp(&a.size)
            .then_with(|| a.extension.cmp(&b.extension))
    });
    out
}
