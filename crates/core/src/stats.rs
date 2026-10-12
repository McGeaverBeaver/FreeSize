//! Summaries computed from a scanned tree.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};

use crate::tree::{NodeId, NodeKind, SizeMode, Tree, flags};

/// The `n` largest files below `under`, largest first.
pub fn largest_files(tree: &Tree, under: NodeId, mode: SizeMode, n: usize) -> Vec<NodeId> {
    largest_files_where(tree, under, mode, n, |_| true)
}

/// Like [`largest_files`], counting only files `keep` accepts.
pub fn largest_files_where(
    tree: &Tree,
    under: NodeId,
    mode: SizeMode,
    n: usize,
    keep: impl Fn(NodeId) -> bool,
) -> Vec<NodeId> {
    if n == 0 {
        return Vec::new();
    }
    // Min-heap holding the best `n` seen so far.
    let mut heap: BinaryHeap<Reverse<(u64, NodeId)>> = BinaryHeap::with_capacity(n + 1);
    for id in tree.descendants(under) {
        let node = tree.node(id);
        if node.kind == NodeKind::Dir || node.has(flags::HARDLINK_DUP) || !keep(id) {
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

/// Whether `id` has the extension `ext` (lower case, without the dot), as
/// [`extension_stats`] groups them; an empty `ext` matches files without one.
pub fn has_extension(tree: &Tree, id: NodeId, ext: &str) -> bool {
    match tree.extension(id) {
        Some(own) => own.eq_ignore_ascii_case(ext.as_bytes()),
        None => ext.is_empty(),
    }
}

/// A group of files in the age or size breakdown: files whose modification
/// time (or size) is in `from..to`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bin {
    pub label: String,
    pub from: i64,
    pub to: i64,
    pub size: u64,
    pub files: u64,
}

/// Year of a time in seconds since 1970 (UTC), and when that year began.
fn year_of(secs: i64) -> (i64, i64) {
    // Howard Hinnant's civil-from-days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, start_of_year(year))
}

/// Seconds since 1970 at the start of 1 January of `year`.
fn start_of_year(year: i64) -> i64 {
    // Days-from-civil for 1 January.
    let y = year - 1;
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = 306; // 1 January counted from 1 March
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400
}

/// Files under `under` by the year they were last changed: this year and
/// each of the 9 before it, then everything older, then files without a
/// date. Empty groups are left out.
pub fn age_bins(tree: &Tree, under: NodeId, mode: SizeMode, now: i64) -> Vec<Bin> {
    let (this_year, _) = year_of(now);
    let oldest = this_year - 9;
    let mut bins: Vec<Bin> = (oldest..=this_year)
        .rev()
        .map(|year| Bin {
            label: year.to_string(),
            from: start_of_year(year),
            to: start_of_year(year + 1),
            size: 0,
            files: 0,
        })
        .collect();
    bins.push(Bin {
        label: format!("Before {oldest}"),
        from: 1,
        to: start_of_year(oldest),
        size: 0,
        files: 0,
    });
    bins.push(Bin {
        label: "No date".into(),
        from: i64::MIN,
        to: 1,
        size: 0,
        files: 0,
    });
    // Files dated in the future count as this year.
    bins[0].to = i64::MAX;
    fill(tree, under, mode, &mut bins, |node| node.modified);
    bins.retain(|b| b.files > 0);
    bins
}

/// Files under `under` by size band, biggest band first. Empty bands are
/// left out.
pub fn size_bins(tree: &Tree, under: NodeId, mode: SizeMode) -> Vec<Bin> {
    const MB: i64 = 1024 * 1024;
    let mut bins: Vec<Bin> = [
        ("1 GB and up", 1024 * MB, i64::MAX),
        ("100 MB to 1 GB", 100 * MB, 1024 * MB),
        ("10 to 100 MB", 10 * MB, 100 * MB),
        ("1 to 10 MB", MB, 10 * MB),
        ("Under 1 MB", i64::MIN, MB),
    ]
    .into_iter()
    .map(|(label, from, to)| Bin {
        label: label.into(),
        from,
        to,
        size: 0,
        files: 0,
    })
    .collect();
    fill(tree, under, mode, &mut bins, |node| {
        i64::try_from(node.size_in(mode)).unwrap_or(i64::MAX)
    });
    bins.retain(|b| b.files > 0);
    bins
}

fn fill(
    tree: &Tree,
    under: NodeId,
    mode: SizeMode,
    bins: &mut [Bin],
    key: impl Fn(&crate::tree::Node) -> i64,
) {
    for id in tree.descendants(under) {
        let node = tree.node(id);
        if node.kind == NodeKind::Dir || node.has(flags::HARDLINK_DUP) {
            continue;
        }
        let k = key(node);
        if let Some(bin) = bins.iter_mut().find(|b| b.from <= k && k < b.to) {
            bin.size += node.size_in(mode);
            bin.files += 1;
        }
    }
}

#[cfg(test)]
mod bin_tests {
    use super::*;

    #[test]
    fn years() {
        assert_eq!(year_of(0), (1970, 0));
        // 2026-10-12
        assert_eq!(year_of(1_791_763_200).0, 2026);
        assert_eq!(start_of_year(2026), 1_767_225_600);
        assert_eq!(year_of(1_767_225_600 - 1).0, 2025);
        assert_eq!(year_of(1_767_225_600).0, 2026);
    }
}
