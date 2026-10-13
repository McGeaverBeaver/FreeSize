//! The rows of the expandable folder tree, as a Slint model.
//!
//! Expanding or collapsing a folder inserts or removes a contiguous block of
//! rows, so the model reports those as single range changes instead of
//! resetting the whole list.

use std::cell::RefCell;

use freesize_core::{NodeId, SizeMode, SortKey, Tree, flags, format_size};
use slint::{Model, ModelNotify, ModelTracker, SharedString};

use crate::TreeRow;

/// Children shown per folder before the rest are folded into one summary row.
const MAX_CHILD_ROWS: usize = 500;

#[derive(Clone, Copy, Debug)]
pub struct RowEntry {
    /// The node shown, or for a summary row the folder whose children it sums up.
    pub node: NodeId,
    pub depth: u16,
    pub expanded: bool,
    pub summary: bool,
}

#[derive(Default)]
pub struct TreeRows {
    items: RefCell<Vec<(RowEntry, TreeRow)>>,
    notify: ModelNotify,
}

impl Model for TreeRows {
    type Data = TreeRow;

    fn row_count(&self) -> usize {
        self.items.borrow().len()
    }

    fn row_data(&self, row: usize) -> Option<TreeRow> {
        self.items.borrow().get(row).map(|(_, data)| data.clone())
    }

    fn model_tracker(&self) -> &dyn ModelTracker {
        &self.notify
    }
}

impl TreeRows {
    pub fn entry(&self, row: usize) -> Option<RowEntry> {
        self.items.borrow().get(row).map(|(entry, _)| *entry)
    }

    pub fn clear(&self) {
        self.replace_all(Vec::new());
    }

    pub fn replace_all(&self, items: Vec<(RowEntry, TreeRow)>) {
        *self.items.borrow_mut() = items;
        self.notify.reset();
    }

    fn insert(&self, at: usize, items: Vec<(RowEntry, TreeRow)>) {
        let count = items.len();
        self.items.borrow_mut().splice(at..at, items);
        if count > 0 {
            self.notify.row_added(at, count);
        }
    }

    fn remove(&self, at: usize, count: usize) {
        self.items.borrow_mut().drain(at..at + count);
        if count > 0 {
            self.notify.row_removed(at, count);
        }
    }

    fn set_expanded(&self, row: usize, expanded: bool) {
        if let Some((entry, data)) = self.items.borrow_mut().get_mut(row) {
            entry.expanded = expanded;
            data.expanded = expanded;
        }
        self.notify.row_changed(row);
    }

    /// Index of the first non-summary row at or after `from` showing `node`.
    pub fn position(&self, node: NodeId, from: usize) -> Option<usize> {
        self.items
            .borrow()
            .iter()
            .skip(from)
            .position(|(e, _)| e.node == node && !e.summary)
            .map(|i| i + from)
    }

    /// Nodes whose rows are currently expanded.
    pub fn expanded_nodes(&self) -> Vec<NodeId> {
        self.items
            .borrow()
            .iter()
            .filter(|(e, _)| e.expanded && !e.summary)
            .map(|(e, _)| e.node)
            .collect()
    }

    /// Expands or collapses the folder at `row`. Returns false if it can't be.
    pub fn toggle(&self, tree: &Tree, mode: SizeMode, row: usize) -> bool {
        let Some(entry) = self.entry(row) else {
            return false;
        };
        if entry.summary || tree.children(entry.node).next().is_none() {
            return false;
        }
        if entry.expanded {
            let count = self.items.borrow()[row + 1..]
                .iter()
                .take_while(|(e, _)| e.depth > entry.depth)
                .count();
            self.remove(row + 1, count);
            self.set_expanded(row, false);
        } else {
            let mut items = Vec::new();
            push_children(tree, mode, entry.node, entry.depth + 1, &[], &mut items);
            self.insert(row + 1, items);
            self.set_expanded(row, true);
        }
        true
    }

    /// Rebuilds all rows from the root, keeping the given folders expanded.
    pub fn rebuild(&self, tree: &Tree, mode: SizeMode, expanded: &[NodeId]) {
        let mut items = Vec::new();
        push_row(tree, mode, tree.root(), 0, expanded, &mut items);
        self.replace_all(items);
    }
}

fn push_row(
    tree: &Tree,
    mode: SizeMode,
    id: NodeId,
    depth: u16,
    expanded: &[NodeId],
    out: &mut Vec<(RowEntry, TreeRow)>,
) {
    let is_expanded = expanded.contains(&id) && tree.children(id).next().is_some();
    out.push(node_row(tree, mode, id, depth, is_expanded));
    if is_expanded {
        push_children(tree, mode, id, depth + 1, expanded, out);
    }
}

fn push_children(
    tree: &Tree,
    mode: SizeMode,
    dir: NodeId,
    depth: u16,
    expanded: &[NodeId],
    out: &mut Vec<(RowEntry, TreeRow)>,
) {
    let children = tree.children_sorted(dir, SortKey::Size(mode));
    let shown = children.len().min(MAX_CHILD_ROWS);
    for &child in &children[..shown] {
        push_row(tree, mode, child, depth, expanded, out);
    }
    let hidden = &children[shown..];
    if !hidden.is_empty() {
        out.push(summary_row(tree, mode, dir, depth, hidden));
    }
}

fn fraction_of_parent(tree: &Tree, mode: SizeMode, id: NodeId, size: u64) -> f32 {
    let parent = tree.node(id).parent;
    if parent == freesize_core::NO_NODE {
        return 1.0;
    }
    let total = tree.node(parent).size_in(mode);
    if total == 0 {
        0.0
    } else {
        (size as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

fn percent(fraction: f32) -> SharedString {
    format!("{:.1}%", fraction * 100.0).into()
}

pub fn node_row(
    tree: &Tree,
    mode: SizeMode,
    id: NodeId,
    depth: u16,
    expanded: bool,
) -> (RowEntry, TreeRow) {
    let node = tree.node(id);
    let size = node.size_in(mode);
    let fraction = fraction_of_parent(tree, mode, id, size);
    let row = TreeRow {
        id: id as i32,
        depth: i32::from(depth),
        name: tree.name(id).as_ref().into(),
        size: format_size(size).into(),
        percent: percent(fraction),
        fraction,
        expandable: node.is_dir() && tree.children(id).next().is_some(),
        expanded,
        is_dir: node.is_dir(),
        summary: false,
        note: note(node.flags).into(),
    };
    let entry = RowEntry {
        node: id,
        depth,
        expanded,
        summary: false,
    };
    (entry, row)
}

fn summary_row(
    tree: &Tree,
    mode: SizeMode,
    dir: NodeId,
    depth: u16,
    hidden: &[NodeId],
) -> (RowEntry, TreeRow) {
    let size: u64 = hidden.iter().map(|&c| tree.node(c).size_in(mode)).sum();
    let total = tree.node(dir).size_in(mode);
    let fraction = if total == 0 {
        0.0
    } else {
        (size as f64 / total as f64) as f32
    };
    let row = TreeRow {
        id: -1,
        depth: i32::from(depth),
        name: format!(
            "… {} smaller items",
            freesize_core::format_count(hidden.len() as u64)
        )
        .into(),
        size: format_size(size).into(),
        percent: percent(fraction),
        fraction,
        expandable: false,
        expanded: false,
        is_dir: false,
        summary: true,
        note: SharedString::new(),
    };
    let entry = RowEntry {
        node: dir,
        depth,
        expanded: false,
        summary: true,
    };
    (entry, row)
}

/// Short warning shown next to an entry's name.
pub fn note(node_flags: u8) -> &'static str {
    if node_flags & flags::ERROR != 0 {
        "no access"
    } else if node_flags & flags::MOUNT_POINT != 0 {
        "other drive"
    } else if node_flags & flags::CLOUD != 0 {
        "cloud only"
    } else if node_flags & flags::HARDLINK_DUP != 0 {
        "hard link"
    } else {
        ""
    }
}
