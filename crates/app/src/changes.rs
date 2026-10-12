//! Rows of the scan view's Changes tab: what grew, what's new, what shrank
//! or went away since an earlier scan of the same folder.

use disktree_core::snapshot::{self, Change, ChangeKind, Snapshot};
use disktree_core::{SizeMode, Tree, format_size};

use crate::CleanRow;

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
    rows.extend(up.iter().take(MAX_ROWS).map(|c| item(tree, root, c)));
    rows.push(heading(
        "Shrank or gone",
        &if down.is_empty() {
            "Nothing shrank noticeably.".to_string()
        } else {
            format!("{} less in these folders.", format_size(sum(&down)))
        },
    ));
    rows.extend(down.iter().take(MAX_ROWS).map(|c| item(tree, root, c)));
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

fn item(tree: &Tree, root: &str, change: &Change) -> CleanRow {
    let (parent, name) = match change.path.rsplit_once('/') {
        Some((parent, name)) => (parent.to_string(), name.to_string()),
        None if change.path.is_empty() => (String::new(), root.to_string()),
        None => (root.to_string(), change.path.clone()),
    };
    let what = match change.kind {
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
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use disktree_core::{ScanOptions, ScanProgress, scan};

    use super::*;

    fn write(root: &Path, path: &str, bytes: usize) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, vec![1u8; bytes]).unwrap();
    }

    #[test]
    fn lists_growth_and_losses() {
        let dir = std::env::temp_dir().join(format!("disktree-changes-{}", std::process::id()));
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
        assert!(rows[2].id > 0);
        assert_eq!(when(10_000, 10_000 - 3 * 3600), "3 hours ago");
        let _ = fs::remove_dir_all(&dir);
    }
}
