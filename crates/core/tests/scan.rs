use std::fs;
use std::path::Path;

use disktree_core::{
    NodeKind, ScanOptions, ScanProgress, SizeMode, SortKey, Tree, extension_stats, flags,
    largest_files, scan,
};

fn write(path: &Path, len: usize) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, vec![b'x'; len]).unwrap();
}

fn scan_dir(path: &Path) -> Tree {
    let result = scan(path, &ScanOptions::default(), &ScanProgress::new()).unwrap();
    assert!(!result.cancelled);
    result.tree
}

fn child(tree: &Tree, parent: u32, name: &str) -> u32 {
    tree.child_by_name(parent, name.as_ref())
        .unwrap_or_else(|| panic!("no child {name}"))
}

#[test]
fn sums_sizes_and_counts() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a/one.bin"), 1000);
    write(&dir.path().join("a/b/two.bin"), 3000);
    write(&dir.path().join("c.txt"), 10);
    fs::create_dir(dir.path().join("empty")).unwrap();

    let tree = scan_dir(dir.path());
    let root = tree.node(tree.root());
    assert_eq!(root.size, 4010);
    assert_eq!(root.files, 3);
    assert_eq!(root.dirs, 3);
    assert!(root.allocated >= root.size || cfg!(windows));

    let a = child(&tree, tree.root(), "a");
    assert_eq!(tree.node(a).size, 4000);
    assert_eq!(tree.node(a).files, 2);
    assert_eq!(tree.node(a).dirs, 1);

    let order: Vec<_> = tree
        .children_sorted(tree.root(), SortKey::Size(SizeMode::Logical))
        .into_iter()
        .map(|c| tree.name(c).into_owned())
        .collect();
    assert_eq!(order, ["a", "c.txt", "empty"]);

    let b = child(&tree, a, "b");
    let two = child(&tree, b, "two.bin");
    assert_eq!(
        tree.path(two),
        std::path::absolute(dir.path()).unwrap().join("a/b/two.bin")
    );
    assert_eq!(tree.depth(two), 3);
    assert_eq!(tree.node(two).kind, NodeKind::File);
}

#[test]
fn children_are_contiguous_and_point_back_to_parent() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..20 {
        write(&dir.path().join(format!("d{}/f{i}", i % 4)), i * 10);
    }
    let tree = scan_dir(dir.path());
    assert_eq!(tree.len(), 1 + 4 + 20);
    for (id, node) in tree.nodes().iter().enumerate() {
        for c in node.children() {
            assert_eq!(tree.node(c).parent as usize, id);
            assert!(c as usize > id);
        }
    }
    assert_eq!(tree.descendants(tree.root()).count(), 24);
}

#[test]
fn largest_files_and_extensions() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("x/movie.MKV"), 5000);
    write(&dir.path().join("y/song.mp3"), 300);
    write(&dir.path().join("y/other.mkv"), 700);
    write(&dir.path().join(".hidden"), 50);
    let tree = scan_dir(dir.path());

    let top: Vec<_> = largest_files(&tree, tree.root(), SizeMode::Logical, 2)
        .into_iter()
        .map(|id| tree.name(id).into_owned())
        .collect();
    assert_eq!(top, ["movie.MKV", "other.mkv"]);

    let exts = extension_stats(&tree, tree.root(), SizeMode::Logical);
    assert_eq!(exts[0].extension, "mkv");
    assert_eq!(exts[0].size, 5700);
    assert_eq!(exts[0].files, 2);
    assert!(exts.iter().any(|e| e.extension.is_empty() && e.size == 50));
}

#[test]
fn mark_deleted_updates_ancestors() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a/b/big.bin"), 9000);
    write(&dir.path().join("a/small.bin"), 100);
    let mut tree = scan_dir(dir.path());

    let a = child(&tree, tree.root(), "a");
    let b = child(&tree, a, "b");
    tree.mark_deleted(b);
    assert_eq!(tree.node(a).size, 100);
    assert_eq!(tree.node(tree.root()).size, 100);
    assert_eq!(tree.node(tree.root()).files, 1);
    assert_eq!(tree.node(tree.root()).dirs, 1);
    assert_eq!(tree.children(a).count(), 1);
    assert!(tree.node(b).has(flags::DELETED));

    // Deleting twice must not subtract twice.
    tree.mark_deleted(b);
    assert_eq!(tree.node(tree.root()).size, 100);
}

#[test]
fn cancelled_scan_returns_partial_tree() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a/b/c.bin"), 10);
    let progress = ScanProgress::new();
    progress.cancel();
    let result = scan(dir.path(), &ScanOptions::default(), &progress).unwrap();
    assert!(result.cancelled);
    assert_eq!(result.tree.len(), 1);
}

#[test]
fn scanning_a_file_gives_a_single_node() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("only.bin");
    write(&file, 1234);
    let tree = scan_dir(&file);
    assert_eq!(tree.len(), 1);
    assert_eq!(tree.node(0).size, 1234);
    assert_eq!(tree.node(0).files, 1);
}

#[test]
fn missing_root_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let err = scan(
        dir.path().join("nope"),
        &ScanOptions::default(),
        &ScanProgress::new(),
    )
    .unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}

#[test]
fn progress_counts_everything() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("a/1"), 10);
    write(&dir.path().join("a/2"), 20);
    write(&dir.path().join("b/3"), 30);
    let progress = ScanProgress::new();
    scan(dir.path(), &ScanOptions::default(), &progress).unwrap();
    let snap = progress.snapshot();
    assert_eq!(snap.files, 3);
    assert_eq!(snap.dirs, 2);
    assert_eq!(snap.bytes, 60);
}

#[cfg(unix)]
mod unix {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn symlinks_are_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("big/data.bin"), 5000);
        symlink(dir.path().join("big"), dir.path().join("link")).unwrap();
        let tree = scan_dir(dir.path());

        let link = child(&tree, tree.root(), "link");
        assert_eq!(tree.node(link).kind, NodeKind::Symlink);
        assert_eq!(tree.node(link).child_count, 0);
        assert_eq!(tree.node(tree.root()).files, 2);
        assert!(tree.node(tree.root()).size < 6000);
    }

    #[test]
    fn hard_links_are_counted_once() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/file.bin"), 8192);
        fs::create_dir(dir.path().join("b")).unwrap();
        fs::hard_link(dir.path().join("a/file.bin"), dir.path().join("b/link.bin")).unwrap();
        let tree = scan_dir(dir.path());

        assert_eq!(tree.node(tree.root()).size, 8192);
        assert_eq!(tree.node(tree.root()).files, 2);
        let dups = tree
            .nodes()
            .iter()
            .filter(|n| n.has(flags::HARDLINK_DUP))
            .count();
        assert_eq!(dups, 1);
    }

    #[test]
    fn unreadable_directories_are_flagged() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("locked/secret.bin"), 100);
        let locked = dir.path().join("locked");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        // Root ignores permissions; nothing to test then.
        let readable = fs::read_dir(&locked).is_ok();
        let result = scan(dir.path(), &ScanOptions::default(), &ScanProgress::new()).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
        if readable {
            return;
        }
        let tree = &result.tree;
        let node = child(tree, tree.root(), "locked");
        assert!(tree.node(node).has(flags::ERROR));
        assert_eq!(result.errors, 1);
    }
}
