//! Compact arena representation of a scanned directory tree.
//!
//! All nodes live in one `Vec` and refer to each other by `u32` index, and all
//! names live in one shared byte buffer. The children of a directory are
//! always stored next to each other, so a directory only needs the index of
//! its first child and a count. This keeps memory at roughly 60–70 bytes per
//! entry, which matters on drives with millions of files.

use std::borrow::Cow;
use std::ffi::OsStr;
use std::ops::Range;
use std::path::PathBuf;

/// Index of a node in a [`Tree`].
pub type NodeId = u32;

/// Marker for "no node" (the root's parent, or a directory without children).
pub const NO_NODE: NodeId = NodeId::MAX;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NodeKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Bit flags stored in [`Node::flags`].
pub mod flags {
    /// The entry or the directory listing could not be read (usually permission denied).
    pub const ERROR: u8 = 1 << 0;
    /// A directory on another filesystem that was not descended into.
    pub const MOUNT_POINT: u8 = 1 << 1;
    /// A hard link to a file that is already counted elsewhere in the tree.
    pub const HARDLINK_DUP: u8 = 1 << 2;
    /// A cloud placeholder (OneDrive, iCloud) whose contents are not stored locally.
    pub const CLOUD: u8 = 1 << 3;
    /// Removed from disk after the scan.
    pub const DELETED: u8 = 1 << 4;
}

/// Which size to report: the file's length, or the space it takes on disk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SizeMode {
    /// File length in bytes ("size").
    Logical,
    /// Bytes allocated on disk ("size on disk").
    #[default]
    Allocated,
}

#[derive(Clone, Debug)]
pub struct Node {
    /// Logical size in bytes; for directories the total of the subtree.
    pub size: u64,
    /// Bytes allocated on disk; for directories the total of the subtree.
    pub allocated: u64,
    /// Last modification time in seconds since the Unix epoch; for
    /// directories the newest time found in the subtree.
    pub modified: i64,
    /// Number of non-directory entries in the subtree (1 for a file).
    pub files: u32,
    /// Number of directories below this node.
    pub dirs: u32,
    pub parent: NodeId,
    pub first_child: NodeId,
    pub child_count: u32,
    pub(crate) name_start: u32,
    pub(crate) name_len: u16,
    pub kind: NodeKind,
    pub flags: u8,
}

impl Node {
    pub(crate) fn new(kind: NodeKind, parent: NodeId) -> Self {
        Self {
            size: 0,
            allocated: 0,
            modified: 0,
            files: 0,
            dirs: 0,
            parent,
            first_child: NO_NODE,
            child_count: 0,
            name_start: 0,
            name_len: 0,
            kind,
            flags: 0,
        }
    }

    pub fn size_in(&self, mode: SizeMode) -> u64 {
        match mode {
            SizeMode::Logical => self.size,
            SizeMode::Allocated => self.allocated,
        }
    }

    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }

    pub fn children(&self) -> Range<NodeId> {
        if self.child_count == 0 {
            0..0
        } else {
            self.first_child..self.first_child + self.child_count
        }
    }
}

/// How to order the children of a directory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Size(SizeMode),
    Name,
    Files,
    Modified,
}

#[derive(Clone, Debug, Default)]
pub struct Tree {
    pub(crate) nodes: Vec<Node>,
    pub(crate) names: Vec<u8>,
}

impl Tree {
    /// Creates a tree containing only a root node named after `root_path`.
    pub(crate) fn with_root(root_path: &OsStr, mut root: Node) -> Self {
        let mut tree = Tree::default();
        let (start, len) = tree.push_name(root_path.as_encoded_bytes());
        root.name_start = start;
        root.name_len = len;
        root.parent = NO_NODE;
        tree.nodes.push(root);
        tree
    }

    pub(crate) fn push_name(&mut self, bytes: &[u8]) -> (u32, u16) {
        let start = u32::try_from(self.names.len()).expect("name buffer exceeds 4 GiB");
        let len = u16::try_from(bytes.len()).expect("file name longer than 64 KiB");
        self.names.extend_from_slice(bytes);
        (start, len)
    }

    pub fn root(&self) -> NodeId {
        0
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn node(&self, id: NodeId) -> &Node {
        &self.nodes[id as usize]
    }

    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    fn name_bytes(&self, id: NodeId) -> &[u8] {
        let node = self.node(id);
        let start = node.name_start as usize;
        &self.names[start..start + node.name_len as usize]
    }

    /// The entry's name for display. The root's name is the scanned path.
    pub fn name(&self, id: NodeId) -> Cow<'_, str> {
        String::from_utf8_lossy(self.name_bytes(id))
    }

    /// The entry's exact name, suitable for building paths.
    pub fn name_os(&self, id: NodeId) -> &OsStr {
        // SAFETY: every name was copied from `OsStr::as_encoded_bytes` in this
        // process and is stored whole, never split.
        unsafe { OsStr::from_encoded_bytes_unchecked(self.name_bytes(id)) }
    }

    /// Lower-cased extension of the entry's name, without the dot.
    pub fn extension(&self, id: NodeId) -> Option<&[u8]> {
        let name = self.name_bytes(id);
        let dot = name.iter().rposition(|&b| b == b'.')?;
        let ext = &name[dot + 1..];
        // ".bashrc" has no extension; very long suffixes are not extensions either.
        (dot > 0 && !ext.is_empty() && ext.len() <= 16).then_some(ext)
    }

    /// Full path of the entry on disk.
    pub fn path(&self, id: NodeId) -> PathBuf {
        let mut chain: Vec<NodeId> = self.ancestors(id).collect();
        chain.reverse();
        let mut path = PathBuf::new();
        for node in chain {
            path.push(self.name_os(node));
        }
        path
    }

    /// `id` followed by its parent, grandparent and so on up to the root.
    pub fn ancestors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        std::iter::successors(Some(id), |&n| {
            let parent = self.node(n).parent;
            (parent != NO_NODE).then_some(parent)
        })
    }

    pub fn depth(&self, id: NodeId) -> usize {
        self.ancestors(id).count() - 1
    }

    /// Children that still exist on disk, in storage order.
    pub fn children(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.node(id)
            .children()
            .filter(|&c| !self.node(c).has(flags::DELETED))
    }

    /// Children that still exist on disk, ordered by `key` (largest first for sizes).
    pub fn children_sorted(&self, id: NodeId, key: SortKey) -> Vec<NodeId> {
        let mut children: Vec<NodeId> = self.children(id).collect();
        match key {
            SortKey::Size(mode) => children.sort_by(|&a, &b| {
                let (na, nb) = (self.node(a), self.node(b));
                nb.size_in(mode)
                    .cmp(&na.size_in(mode))
                    .then_with(|| self.name_bytes(a).cmp(self.name_bytes(b)))
            }),
            SortKey::Name => children.sort_by_cached_key(|&c| self.name(c).to_lowercase()),
            SortKey::Files => children.sort_by_key(|&c| std::cmp::Reverse(self.node(c).files)),
            SortKey::Modified => {
                children.sort_by_key(|&c| std::cmp::Reverse(self.node(c).modified))
            }
        }
        children
    }

    /// Every node below `id` (not including `id`) that still exists on disk.
    pub fn descendants(&self, id: NodeId) -> Descendants<'_> {
        Descendants {
            tree: self,
            stack: vec![self.node(id).children()],
        }
    }

    /// The folders the scan couldn't list, root first and then in storage
    /// order: what a scan with more rights would add. Files whose details
    /// couldn't be read are counted in the scan's errors but not listed here,
    /// since they hold nothing a listing would reveal.
    pub fn unreadable(&self) -> Vec<NodeId> {
        std::iter::once(self.root())
            .chain(self.descendants(self.root()))
            .filter(|&id| {
                let node = self.node(id);
                node.is_dir() && node.has(flags::ERROR)
            })
            .collect()
    }

    /// Finds a direct child of `parent` by exact name.
    pub fn child_by_name(&self, parent: NodeId, name: &OsStr) -> Option<NodeId> {
        let wanted = name.as_encoded_bytes();
        self.children(parent)
            .find(|&c| self.name_bytes(c) == wanted)
    }

    /// Sums every node's totals into its parent. Children always have a higher
    /// index than their parent, so one reverse pass visits each subtree before
    /// the directory that contains it.
    pub(crate) fn aggregate(&mut self) {
        for i in (1..self.nodes.len()).rev() {
            let node = &self.nodes[i];
            let parent = node.parent as usize;
            let counted = !node.has(flags::HARDLINK_DUP);
            let (size, allocated, modified) = (node.size, node.allocated, node.modified);
            let files = node.files;
            let dirs = node.dirs + u32::from(node.is_dir());

            let parent = &mut self.nodes[parent];
            if counted {
                parent.size += size;
                parent.allocated += allocated;
            }
            parent.files += files;
            parent.dirs += dirs;
            parent.modified = parent.modified.max(modified);
        }
    }

    /// Marks `id` as removed from disk and subtracts its totals from every
    /// ancestor. Call this after deleting the entry.
    pub fn mark_deleted(&mut self, id: NodeId) {
        let node = &self.nodes[id as usize];
        if node.has(flags::DELETED) {
            return;
        }
        let counted = !node.has(flags::HARDLINK_DUP);
        let (size, allocated) = if counted {
            (node.size, node.allocated)
        } else {
            (0, 0)
        };
        let files = node.files;
        let dirs = node.dirs + u32::from(node.is_dir());
        let mut parent = node.parent;
        self.nodes[id as usize].flags |= flags::DELETED;

        while parent != NO_NODE {
            let p = &mut self.nodes[parent as usize];
            p.size = p.size.saturating_sub(size);
            p.allocated = p.allocated.saturating_sub(allocated);
            p.files = p.files.saturating_sub(files);
            p.dirs = p.dirs.saturating_sub(dirs);
            parent = p.parent;
        }
    }
}

/// Depth-first iterator returned by [`Tree::descendants`].
pub struct Descendants<'a> {
    tree: &'a Tree,
    stack: Vec<Range<NodeId>>,
}

impl Iterator for Descendants<'_> {
    type Item = NodeId;

    fn next(&mut self) -> Option<NodeId> {
        loop {
            let range = self.stack.last_mut()?;
            let Some(id) = range.next() else {
                self.stack.pop();
                continue;
            };
            let node = self.tree.node(id);
            if node.has(flags::DELETED) {
                continue;
            }
            if node.child_count > 0 {
                self.stack.push(node.children());
            }
            return Some(id);
        }
    }
}
