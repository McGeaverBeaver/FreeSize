//! Scan files in ncdu's JSON export format (version 1.2), so a scan can be
//! saved and opened again, and a server without FreeSize can be scanned
//! with `ncdu -o file.json` and looked at here.
//!
//! The format is a JSON array: `[1, 2, {metadata}, root]`. A directory is
//! an array whose first element describes the directory and the rest are
//! its entries; a file is an object. Sizes are `asize` (length) and `dsize`
//! (on disk).

use std::io::{self, Read, Write};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::tree::{NO_NODE, Node, NodeId, NodeKind, Tree, flags};

/// Writes `tree` as an ncdu export. Deleted entries are left out.
pub fn export(tree: &Tree, timestamp: i64, mut out: impl Write) -> io::Result<()> {
    let root = entry(tree, tree.root(), true);
    let document = json!([
        1,
        2,
        {"progname": "freesize", "progver": env!("CARGO_PKG_VERSION"), "timestamp": timestamp},
        root
    ]);
    serde_json::to_writer(&mut out, &document).map_err(io::Error::other)?;
    out.flush()
}

fn entry(tree: &Tree, id: NodeId, is_root: bool) -> Value {
    let node = tree.node(id);
    let mut info = Map::new();
    let name = if is_root {
        tree.path(id).to_string_lossy().into_owned()
    } else {
        tree.name(id).into_owned()
    };
    info.insert("name".into(), name.into());
    if node.is_dir() {
        // A directory's own size: its total minus what's in it.
        let (inner_size, inner_alloc) = tree.children(id).fold((0, 0), |(s, a), c| {
            let child = tree.node(c);
            if child.has(flags::HARDLINK_DUP) {
                (s, a)
            } else {
                (s + child.size, a + child.allocated)
            }
        });
        info.insert("asize".into(), node.size.saturating_sub(inner_size).into());
        info.insert(
            "dsize".into(),
            node.allocated.saturating_sub(inner_alloc).into(),
        );
    } else {
        info.insert("asize".into(), node.size.into());
        info.insert("dsize".into(), node.allocated.into());
    }
    if node.modified > 0 {
        info.insert("mtime".into(), node.modified.into());
    }
    if node.has(flags::ERROR) {
        info.insert("read_error".into(), true.into());
    }
    if node.has(flags::MOUNT_POINT) {
        info.insert("excluded".into(), "otherfs".into());
    }
    if node.has(flags::HARDLINK_DUP) {
        info.insert("hlnkc".into(), true.into());
    }
    if matches!(node.kind, NodeKind::Symlink | NodeKind::Other) {
        info.insert("notreg".into(), true.into());
    }
    if !node.is_dir() {
        return Value::Object(info);
    }
    let mut items = vec![Value::Object(info)];
    items.extend(tree.children(id).map(|child| entry(tree, child, false)));
    Value::Array(items)
}

/// One entry read from a file, before it becomes a tree node.
struct Item {
    name: String,
    dir: bool,
    size: u64,
    allocated: u64,
    modified: i64,
    flags: u8,
    kind: NodeKind,
    children: Vec<Item>,
}

/// Reads an ncdu export (from ncdu 1.x, ncdu 2.x's JSON export, or
/// FreeSize).
pub fn import(input: impl Read) -> io::Result<Tree> {
    import_with_time(input).map(|(tree, _)| tree)
}

/// Reads an ncdu export, with when it was made (seconds since 1970) if the
/// file says.
pub fn import_with_time(input: impl Read) -> io::Result<(Tree, Option<i64>)> {
    let bad = |what: &str| io::Error::new(io::ErrorKind::InvalidData, what.to_string());
    let document: Value =
        serde_json::from_reader(input).map_err(|e| bad(&format!("not a saved scan ({e})")))?;
    let parts = document
        .as_array()
        .ok_or_else(|| bad("not an ncdu export"))?;
    if parts.first().and_then(Value::as_u64) != Some(1) || parts.len() < 4 {
        return Err(bad("not an ncdu export (version 1)"));
    }
    let timestamp = parts[2]
        .get("timestamp")
        .and_then(Value::as_i64)
        .filter(|&t| t > 0);
    let root = item(&parts[3]).ok_or_else(|| bad("the export has no root folder"))?;
    Ok((build(root), timestamp))
}

fn item(value: &Value) -> Option<Item> {
    let (info, children) = match value {
        Value::Array(items) => (items.first()?.as_object()?, &items[1..]),
        Value::Object(info) => (info, &[][..]),
        _ => return None,
    };
    let dir = value.is_array();
    let flag = |key: &str| info.get(key).is_some_and(|v| v.as_bool().unwrap_or(true));
    let mut node_flags = 0;
    if flag("read_error") {
        node_flags |= flags::ERROR;
    }
    if info.get("excluded").and_then(Value::as_str) == Some("otherfs") {
        node_flags |= flags::MOUNT_POINT;
    }
    if flag("hlnkc") {
        node_flags |= flags::HARDLINK_DUP;
    }
    Some(Item {
        name: info.get("name")?.as_str()?.to_string(),
        dir,
        size: info.get("asize").and_then(Value::as_u64).unwrap_or(0),
        allocated: info.get("dsize").and_then(Value::as_u64).unwrap_or(0),
        modified: info.get("mtime").and_then(Value::as_i64).unwrap_or(0),
        flags: node_flags,
        kind: if dir {
            NodeKind::Dir
        } else if flag("notreg") {
            NodeKind::Other
        } else {
            NodeKind::File
        },
        children: children.iter().filter_map(item).collect(),
    })
}

/// Lays the items out as the scanner does: each directory's children next
/// to each other, then totals summed up the tree.
fn build(root: Item) -> Tree {
    let node = |item: &Item, parent: NodeId| {
        let mut node = Node::new(item.kind, parent);
        node.size = item.size;
        node.allocated = item.allocated;
        node.modified = item.modified;
        node.flags = item.flags;
        node.files = u32::from(!item.dir);
        node
    };
    let mut tree = Tree::with_root(Path::new(&root.name).as_os_str(), node(&root, NO_NODE));
    let mut queue: std::collections::VecDeque<(NodeId, Vec<Item>)> =
        std::collections::VecDeque::from([(tree.root(), root.children)]);
    while let Some((parent, children)) = queue.pop_front() {
        if children.is_empty() {
            continue;
        }
        let first = tree.nodes.len() as NodeId;
        tree.nodes[parent as usize].first_child = first;
        tree.nodes[parent as usize].child_count = children.len() as u32;
        let mut subdirs = Vec::new();
        for mut child in children {
            let mut n = node(&child, parent);
            let (start, len) = tree.push_name(child.name.as_bytes());
            n.name_start = start;
            n.name_len = len;
            let id = tree.nodes.len() as NodeId;
            tree.nodes.push(n);
            if child.dir {
                subdirs.push((id, std::mem::take(&mut child.children)));
            }
        }
        queue.extend(subdirs);
    }
    tree.aggregate();
    tree
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{ScanOptions, ScanProgress, SizeMode, largest_files, scan};

    #[test]
    fn round_trips_a_scan() {
        let dir = tempfile::tempdir().unwrap();
        let write = |path: &str, bytes: usize| {
            let path = dir.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![1u8; bytes]).unwrap();
        };
        write("a/big.bin", 5000);
        write("a/b/small.txt", 10);
        write("c.mp4", 3000);
        let tree = scan(dir.path(), &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let mut bytes = Vec::new();
        export(&tree, 1_790_000_000, &mut bytes).unwrap();
        let back = import(bytes.as_slice()).unwrap();

        assert_eq!(back.len(), tree.len());
        assert_eq!(back.path(back.root()), tree.path(tree.root()));
        let root = (tree.node(tree.root()), back.node(back.root()));
        assert_eq!(root.0.size, root.1.size);
        assert_eq!(root.0.allocated, root.1.allocated);
        assert_eq!(root.0.files, root.1.files);
        assert_eq!(root.0.dirs, root.1.dirs);
        let names = |t: &Tree| -> Vec<String> {
            largest_files(t, t.root(), SizeMode::Logical, 10)
                .into_iter()
                .map(|id| t.name(id).into_owned())
                .collect()
        };
        assert_eq!(names(&back), names(&tree));
    }

    #[test]
    fn reads_ncdu_output() {
        let text = r#"[1,2,{"progname":"ncdu","progver":"1.19","timestamp":1700000000},
            [{"name":"/srv","asize":4096,"dsize":4096,"dev":2049},
             {"name":"movie.mkv","asize":7000,"dsize":8192,"ino":12},
             [{"name":"logs","asize":4096,"dsize":4096},
              {"name":"a.log","asize":100,"dsize":4096},
              {"name":"secret","read_error":true}],
             {"name":"link","asize":12,"dsize":0,"notreg":true},
             {"name":"proc","excluded":"otherfs"}]]"#;
        let tree = import(text.as_bytes()).unwrap();
        let root = tree.node(tree.root());
        assert_eq!(tree.name(tree.root()), "/srv");
        assert_eq!(root.size, 4096 + 7000 + 4096 + 100 + 12);
        assert_eq!(root.files, 5);
        let logs = tree.child_by_name(tree.root(), "logs".as_ref()).unwrap();
        let secret = tree.child_by_name(logs, "secret".as_ref()).unwrap();
        assert!(tree.node(secret).has(flags::ERROR));
        let (_, taken) = import_with_time(text.as_bytes()).unwrap();
        assert_eq!(taken, Some(1_700_000_000));
        assert!(import(&b"[2, 0]"[..]).is_err());
        assert!(import(&b"not json"[..]).is_err());
    }
}
