//! Rows of the scan view's Changes tab: what grew, what's new, what shrank
//! or went away since an earlier scan of the same folder.

use freesize_core::snapshot::{self, Change, ChangeKind, Snapshot};
use freesize_core::{SizeMode, Tree, format_size};

use crate::{ChartPoint, CleanRow};

const HEADING: i32 = 0;
const ITEM: i32 = 1;
const MAX_ROWS: usize = 100;
const MB: u64 = 1024 * 1024;

/// Changes smaller than this aren't listed: a thousandth of the folder,
/// between 1 MB and 50 MB.
pub fn min_change(total: u64) -> u64 {
    (total / 1000).clamp(MB, 50 * MB)
}

/// "3 hours ago" for recent times, else the date.
pub fn when(now: i64, then: i64) -> String {
    let secs = (now - then).max(0);
    match secs {
        s if s < 2 * 60 => "just now".into(),
        s if s < 60 * 60 => format!("{} minutes ago", s / 60),
        s if s < 2 * 60 * 60 => "an hour ago".into(),
        s if s < 24 * 60 * 60 => format!("{} hours ago", s / 3600),
        _ => crate::insights::date(then.max(0) as u64),
    }
}

/// The rows comparing `tree` now with `earlier`. `root` names the scanned
/// folder.
pub fn rows(
    tree: &Tree,
    root: &str,
    earlier: &Snapshot,
    mode: SizeMode,
    now: i64,
) -> Vec<CleanRow> {
    let current = snapshot::take(tree, now, crate::scans::MAX_FOLDERS);
    let total = current.total(mode);
    let changes = snapshot::compare(earlier, &current, mode, min_change(total));
    let (up, down): (Vec<&Change>, Vec<&Change>) = changes.iter().partition(|c| c.delta() > 0);
    let sum = |list: &[&Change]| list.iter().map(|c| c.delta().unsigned_abs()).sum::<u64>();
    let was = earlier.total(mode);

    let mut rows = vec![heading(
        "Since then",
        &format!(
            "{} then, {} now: {}{}.",
            format_size(was),
            format_size(total),
            if total >= was { "+" } else { "−" },
            format_size(total.abs_diff(was))
        ),
    )];
    rows.push(heading(
        "Grew or new",
        &if up.is_empty() {
            "Nothing grew noticeably.".to_string()
        } else {
            format!("{} more in these folders.", format_size(sum(&up)))
        },
    ));
    // Bars against the biggest listed change, either way.
    let biggest = changes
        .iter()
        .map(|c| c.delta().unsigned_abs())
        .max()
        .unwrap_or(1)
        .max(1);
    let item = |c: &&Change| item(tree, root, c, earlier.floor, biggest);
    rows.extend(up.iter().take(MAX_ROWS).map(item));
    rows.push(heading(
        "Shrank or gone",
        &if down.is_empty() {
            "Nothing shrank noticeably.".to_string()
        } else {
            format!("{} less in these folders.", format_size(sum(&down)))
        },
    ));
    rows.extend(down.iter().take(MAX_ROWS).map(item));
    rows
}

fn heading(title: &str, subtitle: &str) -> CleanRow {
    CleanRow {
        kind: HEADING,
        id: -1,
        title: title.into(),
        subtitle: subtitle.into(),
        ..Default::default()
    }
}

fn item(tree: &Tree, root: &str, change: &Change, floor: u64, biggest: u64) -> CleanRow {
    let (parent, name) = match change.path.rsplit_once('/') {
        Some((parent, name)) => (parent.to_string(), name.to_string()),
        None if change.path.is_empty() => (String::new(), root.to_string()),
        None => (root.to_string(), change.path.clone()),
    };
    let what = match change.kind {
        // An older scan kept only its bigger folders: this one may have
        // been there, smaller.
        ChangeKind::New if floor > 0 => format!("new, or under {} then", format_size(floor)),
        ChangeKind::New => "new".to_string(),
        ChangeKind::Gone => format!("gone, was {}", format_size(change.before)),
        ChangeKind::Grew | ChangeKind::Shrank => format!(
            "{} → {}",
            format_size(change.before),
            format_size(change.after)
        ),
    };
    let sign = if change.delta() >= 0 { "+" } else { "−" };
    CleanRow {
        kind: ITEM,
        // Gone folders can't be selected.
        id: snapshot::find(tree, &change.path).map_or(-1, |id| id as i32),
        title: name.into(),
        subtitle: if parent.is_empty() {
            what
        } else {
            format!("{parent} · {what}")
        }
        .into(),
        detail: format!("{sign}{}", format_size(change.delta().unsigned_abs())).into(),
        bar: change.delta() as f32 / biggest as f32,
        ..Default::default()
    }
}

/// The Changes tab's chart: the folder's total at each kept scan and now.
#[derive(Debug, Default, PartialEq)]
pub struct Chart {
    /// SVG path of the line, in pixels of the plot (y grows down).
    pub line: String,
    /// Each scan, oldest first, then now. `index` is the scan's place in the
    /// picker (newest first), -1 for now.
    pub points: Vec<ChartPoint>,
    /// The highest and lowest totals, and the first and last dates.
    pub high: String,
    pub low: String,
    pub start: String,
    pub end: String,
}

/// The chart for `earlier` scans (oldest first) and `total` now, with the
/// scan at `compare` (an index into `earlier`) marked, for a plot `size`
/// pixels wide and high. `None` without earlier scans.
pub fn chart(
    earlier: &[Snapshot],
    total: u64,
    now: i64,
    mode: SizeMode,
    compare: Option<usize>,
    size: (f32, f32),
) -> Option<Chart> {
    if earlier.is_empty() {
        return None;
    }
    let values: Vec<(i64, u64)> = earlier
        .iter()
        .map(|s| (s.taken, s.total(mode)))
        .chain(std::iter::once((now, total)))
        .collect();
    let (lo, hi) = values
        .iter()
        .fold((u64::MAX, 0), |(lo, hi), &(_, v)| (lo.min(v), hi.max(v)));
    // A flat line sits in the middle rather than on an edge.
    let span = (hi - lo).max(1) as f64;
    let (first, last) = (values[0].0, values[values.len() - 1].0);
    let count = values.len();
    let points: Vec<ChartPoint> = values
        .iter()
        .enumerate()
        .map(|(i, &(taken, v))| {
            let x = if last > first {
                (taken - first) as f64 / (last - first) as f64
            } else {
                i as f64 / (count - 1) as f64
            };
            let y = if hi > lo {
                0.92 - 0.84 * (v - lo) as f64 / span
            } else {
                0.5
            };
            let is_now = i + 1 == count;
            ChartPoint {
                x: x as f32,
                y: y as f32,
                label: format!(
                    "{}: {}",
                    if is_now {
                        "This scan".to_string()
                    } else {
                        when(now, taken)
                    },
                    format_size(v)
                )
                .into(),
                index: if is_now { -1 } else { (count - 2 - i) as i32 },
                selected: compare == Some(i),
            }
        })
        .collect();
    let line = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            format!(
                "{} {:.1} {:.1}",
                if i == 0 { "M" } else { "L" },
                p.x * size.0,
                p.y * size.1
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    Some(Chart {
        line,
        points,
        high: format_size(hi),
        low: format_size(lo),
        start: when(now, first),
        end: "this scan".into(),
    })
}

/// The scan to compare with at first: the newest at least a month older
/// than `now`, else the oldest, as on the Hub.
pub fn default_compare(earlier: &[Snapshot], now: i64) -> Option<usize> {
    const MONTH: i64 = 30 * 24 * 60 * 60;
    earlier
        .iter()
        .rposition(|s| s.taken <= now - MONTH)
        .or(if earlier.is_empty() { None } else { Some(0) })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use freesize_core::{ScanOptions, ScanProgress, scan};

    use super::*;

    fn write(root: &Path, path: &str, bytes: usize) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; bytes]).unwrap();
    }

    #[test]
    fn lists_growth_and_losses() {
        let dir = std::env::temp_dir().join(format!("freesize-changes-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        write(&dir, "Old/x.bin", 3 * MB as usize);
        write(&dir, "Pictures/a.jpg", MB as usize);
        let scan_now = || {
            scan(&dir, &ScanOptions::default(), &ScanProgress::new())
                .unwrap()
                .tree
        };
        let earlier = snapshot::take(&scan_now(), 100, 1000);
        write(&dir, "Pictures/b.jpg", 5 * MB as usize);
        fs::remove_dir_all(dir.join("Old")).unwrap();
        let tree = scan_now();

        let rows = rows(&tree, "phone", &earlier, SizeMode::Logical, 200);
        let text: Vec<(String, String, String)> = rows
            .iter()
            .map(|r| {
                (
                    r.title.to_string(),
                    r.subtitle.to_string(),
                    r.detail.to_string(),
                )
            })
            .collect();
        assert_eq!(text[0].1, "4.00 MB then, 6.00 MB now: +2.00 MB.");
        assert_eq!(
            text[2],
            (
                "Pictures".into(),
                "phone · 1.00 MB → 6.00 MB".into(),
                "+5.00 MB".into()
            )
        );
        assert_eq!(
            text[4],
            (
                "Old".into(),
                "phone · gone, was 3.00 MB".into(),
                "−3.00 MB".into()
            )
        );
        assert_eq!(rows[4].id, -1);
        assert_eq!(rows[2].bar, 1.0);
        assert!((rows[4].bar + 0.6).abs() < 0.001);
        assert!(rows[2].id > 0);
        assert_eq!(when(10_000, 10_000 - 3 * 3600), "3 hours ago");

        // The chart: the earlier scan, then now, higher.
        let c = chart(
            std::slice::from_ref(&earlier),
            6 * MB,
            200,
            SizeMode::Logical,
            Some(0),
            (1000.0, 100.0),
        )
        .unwrap();
        assert_eq!(c.line, "M 0.0 92.0 L 1000.0 8.0");
        assert_eq!(c.points[0].index, 0);
        assert!(c.points[0].selected);
        assert_eq!(c.points[1].index, -1);
        assert_eq!(c.points[1].label.as_str(), "This scan: 6.00 MB");
        assert_eq!((c.high.as_str(), c.low.as_str()), ("6.00 MB", "4.00 MB"));
        assert_eq!(chart(&[], 1, 1, SizeMode::Logical, None, (1.0, 1.0)), None);
        assert_eq!(
            default_compare(std::slice::from_ref(&earlier), 200),
            Some(0)
        );
        let month = 30 * 24 * 3600;
        let old = |taken| Snapshot {
            taken,
            ..earlier.clone()
        };
        assert_eq!(
            default_compare(&[old(0), old(10), old(month + 50)], month + 100),
            Some(1)
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
