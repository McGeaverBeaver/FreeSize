//! The map: a treemap or bubbles of what fills the array, one disk or
//! pool, or one share, built when the page asks for it and never by the
//! scheduled look.
//!
//! Each disk is scanned on its own (`/mnt/disk1`, never `/mnt/user` on top),
//! so every file is counted once, on the disk it's really on. Its tree is
//! merged into the map by path and then let go, so a folder living on
//! several disks is one box with their sizes added, and only one disk's
//! files sit in memory at a time. What's kept is bounded too: the biggest
//! [`Limits::files`] files get boxes of their own and the rest are summed
//! per folder ("smaller files"), and past [`Limits::folders`] folders the
//! smallest are kept closed, as one box each.
//!
//! The finished map goes to a file; the page asks for one view of it at a
//! time ([`Map::view`]): the boxes or bubbles for one folder at one size.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use freesize_core::bubbles::{self, Circle};
use freesize_core::treemap::{self, Rect};
use freesize_core::{NodeId, ScanOptions, ScanProgress, Tree, format_count, scan};
use freesize_proto::unraid::DiskKind;
use serde::{Deserialize, Serialize};

use crate::System;
use crate::array::Array;

/// The folder on a pool where the map's file is kept.
pub const MAP_DIR: &str = ".freesize";
const MAP_FILE: &str = "map.dat";

/// What a map covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Every data disk and pool, as `/mnt/user` shows them.
    All,
    /// One data disk or pool.
    Disk(String),
    /// One share, on every disk holding it.
    Share(String),
}

impl Target {
    /// From `all`, `disk:disk1` or `share:Movies`.
    pub fn parse(text: &str) -> Result<Target, String> {
        let bad = || "choose all, a disk (disk:disk1) or a share (share:Movies)".to_string();
        if text == "all" {
            return Ok(Target::All);
        }
        let (kind, name) = text.split_once(':').ok_or_else(bad)?;
        if name.is_empty()
            || name.contains('/')
            || name.contains('\0')
            || name == "."
            || name == ".."
        {
            return Err(bad());
        }
        match kind {
            "disk" => Ok(Target::Disk(name.to_string())),
            "share" => Ok(Target::Share(name.to_string())),
            _ => Err(bad()),
        }
    }

    /// What [`Target::parse`] reads.
    pub fn code(&self) -> String {
        match self {
            Target::All => "all".into(),
            Target::Disk(name) => format!("disk:{name}"),
            Target::Share(name) => format!("share:{name}"),
        }
    }

    /// For a sentence: "the whole array", "disk1", "the Movies share".
    pub fn label(&self) -> String {
        match self {
            Target::All => "the whole array".into(),
            Target::Disk(name) => name.clone(),
            Target::Share(name) => format!("the {name} share"),
        }
    }
}

/// How much a map keeps.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Most files with a box of their own.
    pub files: usize,
    /// Most folders that can be opened.
    pub folders: usize,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits {
            files: 200_000,
            folders: 200_000,
        }
    }
}

/// What a map is of, and how it came out.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    /// [`Target::code`].
    pub target: String,
    /// The server path of the map's top: `/mnt/user`, `/mnt/disk1`,
    /// `/mnt/user/Movies`.
    pub root: String,
    pub built: i64,
    /// How long building took.
    pub seconds: u64,
    /// Bytes on disk.
    pub size: u64,
    pub files: u64,
    pub folders: u64,
    /// Files smaller than this are summed per folder; 0 when every file
    /// has its own box.
    pub floor: u64,
    /// Folders smaller than this are kept closed; 0 when none is.
    pub folder_floor: u64,
    /// The disks it covers; a node's sizes per disk index into this.
    pub disks: Vec<String>,
    /// Disks left out, and why.
    pub missed: Vec<String>,
    /// Folders or files that couldn't be read.
    pub unreadable: u64,
    /// The most memory the background part has used, after building.
    pub peak_memory: Option<u64>,
}

/// Bytes on each disk, by index into [`Meta::disks`].
type OnDisks = Vec<(u8, u64)>;

fn add_on(on: &mut OnDisks, disk: u8, bytes: u64) {
    if bytes == 0 {
        return;
    }
    match on.iter_mut().find(|(d, _)| *d == disk) {
        Some((_, total)) => *total += bytes,
        None => on.push((disk, bytes)),
    }
}

fn add_all(on: &mut OnDisks, more: &OnDisks) {
    for &(disk, bytes) in more {
        add_on(on, disk, bytes);
    }
}

fn sum(on: &OnDisks) -> u64 {
    on.iter().map(|&(_, bytes)| bytes).sum()
}

// ----- Building ----------------------------------------------------------

struct Building {
    name: String,
    parent: u32,
    dir: bool,
    /// A folder kept as one box: what's in it isn't kept.
    closed: bool,
    dead: bool,
    /// A file's or closed folder's bytes per disk.
    on: OnDisks,
    /// A closed folder's files.
    files: u64,
    /// An open folder's files without a box of their own.
    small: OnDisks,
    small_count: u64,
}

impl Building {
    fn new(name: String, parent: u32, dir: bool) -> Building {
        Building {
            name,
            parent,
            dir,
            closed: false,
            dead: false,
            on: Vec::new(),
            files: 0,
            small: Vec::new(),
            small_count: 0,
        }
    }
}

/// What a node holds in all, itself included.
#[derive(Clone, Default)]
struct Total {
    on: OnDisks,
    files: u64,
}

/// A map being put together, a disk at a time.
pub struct Builder {
    limits: Limits,
    nodes: Vec<Building>,
    /// A folder's child by (parent, is a folder, name).
    index: HashMap<(u32, bool, String), u32>,
    kept_files: usize,
    open_dirs: usize,
    dead: usize,
    floor: u64,
    folder_floor: u64,
    disks: Vec<String>,
}

impl Builder {
    pub fn new(limits: Limits) -> Builder {
        Builder {
            limits,
            nodes: vec![Building::new(String::new(), 0, true)],
            index: HashMap::new(),
            kept_files: 0,
            open_dirs: 0,
            dead: 0,
            floor: 0,
            folder_floor: 0,
            disks: Vec::new(),
        }
    }

    /// Merges what's below `from` in a disk's scan into the map's top.
    pub fn add_disk(&mut self, disk: &str, tree: &Tree, from: NodeId) {
        let d = match self.disks.iter().position(|name| name == disk) {
            Some(i) => i,
            None => {
                self.disks.push(disk.to_string());
                self.disks.len() - 1
            }
        }
        .min(usize::from(u8::MAX)) as u8;
        self.make_room(tree, from);

        let mut stack = vec![(from, 0u32)];
        while let Some((dir, into)) = stack.pop() {
            for child in tree.children(dir) {
                let node = tree.node(child);
                let name = tree.name(child).into_owned();
                if node.is_dir() {
                    let key = (into, true, name);
                    let existing = self.index.get(&key).copied();
                    let id = match existing {
                        Some(id) => id,
                        None => {
                            let id = self.nodes.len() as u32;
                            let mut made = Building::new(key.2.clone(), into, true);
                            made.closed = node.allocated < self.folder_floor;
                            if !made.closed {
                                self.open_dirs += 1;
                            }
                            self.nodes.push(made);
                            self.index.insert(key, id);
                            id
                        }
                    };
                    let building = &mut self.nodes[id as usize];
                    if building.closed {
                        // The files only, as open folders are added up:
                        // the tree's own total has the folders' entries.
                        let bytes = tree
                            .descendants(child)
                            .map(|id| tree.node(id))
                            .filter(|n| !n.is_dir())
                            .map(|n| n.allocated)
                            .sum();
                        add_on(&mut building.on, d, bytes);
                        building.files += u64::from(node.files);
                    } else {
                        stack.push((child, id));
                    }
                } else if node.allocated > 0 && node.allocated >= self.floor {
                    let key = (into, false, name);
                    let id = match self.index.get(&key).copied() {
                        Some(id) => id,
                        None => {
                            let id = self.nodes.len() as u32;
                            self.nodes.push(Building::new(key.2.clone(), into, false));
                            self.index.insert(key, id);
                            self.kept_files += 1;
                            id
                        }
                    };
                    add_on(&mut self.nodes[id as usize].on, d, node.allocated);
                } else {
                    let building = &mut self.nodes[into as usize];
                    add_on(&mut building.small, d, node.allocated);
                    building.small_count += 1;
                }
            }
        }
        if self.dead > self.nodes.len() / 2 {
            self.compact();
        }
    }

    /// Before a disk goes in: raises the floors so that it fits, and
    /// folds what's already kept below them.
    fn make_room(&mut self, tree: &Tree, from: NodeId) {
        let coming = tree.node(from);
        if self.kept_files + coming.files as usize > self.limits.files {
            let mut sizes: Vec<u64> = self
                .nodes
                .iter()
                .filter(|n| !n.dead && !n.dir)
                .map(|n| sum(&n.on))
                .chain(
                    tree.descendants(from)
                        .map(|id| tree.node(id))
                        .filter(|n| !n.is_dir() && n.allocated > 0)
                        .map(|n| n.allocated),
                )
                .collect();
            if let Some(floor) = floor_for(&mut sizes, self.limits.files)
                && floor > self.floor
            {
                self.floor = floor;
                self.fold_files();
            }
        }
        if self.open_dirs + coming.dirs as usize > self.limits.folders {
            let totals = self.totals();
            let mut sizes: Vec<u64> = self
                .nodes
                .iter()
                .enumerate()
                .skip(1)
                .filter(|(_, n)| !n.dead && n.dir && !n.closed)
                .map(|(i, _)| sum(&totals[i].on))
                .chain(
                    tree.descendants(from)
                        .map(|id| tree.node(id))
                        .filter(|n| n.is_dir())
                        .map(|n| n.allocated),
                )
                .collect();
            if let Some(floor) = floor_for(&mut sizes, self.limits.folders)
                && floor > self.folder_floor
            {
                self.folder_floor = floor;
                self.close_folders(&totals);
            }
        }
    }

    /// Files below the floor join their folder's smaller files.
    fn fold_files(&mut self) {
        for i in 1..self.nodes.len() {
            let node = &self.nodes[i];
            if node.dead || node.dir || sum(&node.on) >= self.floor {
                continue;
            }
            let (parent, on) = (node.parent as usize, node.on.clone());
            add_all(&mut self.nodes[parent].small, &on);
            self.nodes[parent].small_count += 1;
            self.bury(i);
        }
    }

    /// Open folders below the folder floor become one box each.
    fn close_folders(&mut self, totals: &[Total]) {
        // Children come after their parents, so one pass forward reaches
        // everything below a folder after the folder itself.
        let mut gone = vec![false; self.nodes.len()];
        for i in 1..self.nodes.len() {
            if self.nodes[i].dead {
                continue;
            }
            if gone[self.nodes[i].parent as usize] {
                gone[i] = true;
                self.bury(i);
                continue;
            }
            let node = &mut self.nodes[i];
            if node.dir && !node.closed && sum(&totals[i].on) < self.folder_floor {
                node.closed = true;
                node.on = totals[i].on.clone();
                node.files = totals[i].files;
                node.small = Vec::new();
                node.small_count = 0;
                self.open_dirs -= 1;
                gone[i] = true;
            }
        }
    }

    fn bury(&mut self, i: usize) {
        let node = &mut self.nodes[i];
        node.dead = true;
        node.on = Vec::new();
        node.small = Vec::new();
        let key = (node.parent, node.dir, std::mem::take(&mut node.name));
        if node.dir {
            if !node.closed {
                self.open_dirs -= 1;
            }
        } else {
            self.kept_files -= 1;
        }
        self.index.remove(&key);
        self.dead += 1;
    }

    /// Drops the buried nodes.
    fn compact(&mut self) {
        let mut moved = vec![0u32; self.nodes.len()];
        let mut kept = Vec::with_capacity(self.nodes.len() - self.dead);
        for (i, mut node) in std::mem::take(&mut self.nodes).into_iter().enumerate() {
            if node.dead {
                continue;
            }
            moved[i] = kept.len() as u32;
            node.parent = moved[node.parent as usize];
            kept.push(node);
        }
        self.nodes = kept;
        self.dead = 0;
        self.index = self
            .nodes
            .iter()
            .enumerate()
            .skip(1)
            .map(|(i, n)| ((n.parent, n.dir, n.name.clone()), i as u32))
            .collect();
    }

    /// What every node holds, itself and everything below.
    fn totals(&self) -> Vec<Total> {
        let mut totals = vec![Total::default(); self.nodes.len()];
        for i in (0..self.nodes.len()).rev() {
            let node = &self.nodes[i];
            if node.dead {
                continue;
            }
            if !node.dir {
                totals[i].on = node.on.clone();
                totals[i].files = 1;
            } else if node.closed {
                totals[i].on = node.on.clone();
                totals[i].files = node.files;
            } else {
                add_all(&mut totals[i].on, &node.small);
                totals[i].files += node.small_count;
            }
            if i > 0 {
                let own = std::mem::take(&mut totals[i]);
                let parent = &mut totals[node.parent as usize];
                add_all(&mut parent.on, &own.on);
                parent.files += own.files;
                totals[i] = own;
            }
        }
        totals
    }

    /// The finished map: folders before what's in them, and each folder's
    /// contents together, biggest first.
    pub fn finish(mut self, mut meta: Meta) -> Map {
        if self.dead > 0 {
            self.compact();
        }
        let totals = self.totals();
        let mut children: Vec<Vec<u32>> = vec![Vec::new(); self.nodes.len()];
        for (i, node) in self.nodes.iter().enumerate().skip(1) {
            children[node.parent as usize].push(i as u32);
        }
        let mut nodes: Vec<Node> = Vec::with_capacity(self.nodes.len());
        nodes.push(Node {
            name: String::new(),
            kind: Kind::Folder,
            size: sum(&totals[0].on),
            files: totals[0].files,
            on: totals[0].on.clone(),
            first: 0,
            count: 0,
        });
        let mut queue: VecDeque<(usize, u32)> = VecDeque::from([(0usize, 0u32)]);
        let mut folders = 0;
        while let Some((from, to)) = queue.pop_front() {
            let building = &self.nodes[from];
            let mut made: Vec<(Option<usize>, Node)> = children[from]
                .iter()
                .map(|&c| {
                    let c = c as usize;
                    let child = &self.nodes[c];
                    let kind = match (child.dir, child.closed) {
                        (false, _) => Kind::File,
                        (true, false) => Kind::Folder,
                        (true, true) => Kind::Closed,
                    };
                    let node = Node {
                        name: child.name.clone(),
                        kind,
                        size: sum(&totals[c].on),
                        files: totals[c].files,
                        on: totals[c].on.clone(),
                        first: 0,
                        count: 0,
                    };
                    ((kind == Kind::Folder).then_some(c), node)
                })
                .collect();
            if sum(&building.small) > 0 {
                made.push((
                    None,
                    Node {
                        name: String::new(),
                        kind: Kind::Smaller,
                        size: sum(&building.small),
                        files: building.small_count,
                        on: building.small.clone(),
                        first: 0,
                        count: 0,
                    },
                ));
            }
            made.sort_by(|a, b| {
                b.1.size
                    .cmp(&a.1.size)
                    .then_with(|| a.1.name.cmp(&b.1.name))
            });
            nodes[to as usize].first = nodes.len() as u32;
            nodes[to as usize].count = made.len() as u32;
            for (open, node) in made {
                if node.kind != Kind::File && node.kind != Kind::Smaller {
                    folders += 1;
                }
                if let Some(open) = open {
                    queue.push_back((open, nodes.len() as u32));
                }
                nodes.push(node);
            }
        }
        meta.size = nodes[0].size;
        meta.files = nodes[0].files;
        meta.folders = folders;
        meta.floor = self.floor;
        meta.folder_floor = self.folder_floor;
        meta.disks = self.disks;
        Map { meta, nodes }
    }
}

/// The size below which things are folded so that about three quarters of
/// `limit` stay (room for the next disk); none while everything fits.
fn floor_for(sizes: &mut [u64], limit: usize) -> Option<u64> {
    if sizes.len() <= limit {
        return None;
    }
    let keep = (limit * 3 / 4).max(1);
    let (_, smallest_kept, _) = sizes.select_nth_unstable_by(keep - 1, |a, b| b.cmp(a));
    Some(*smallest_kept)
}

// ----- The finished map --------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    /// A folder that opens.
    Folder,
    /// A folder's files without boxes of their own, together.
    Smaller,
    /// A folder too small to keep what's in it.
    Closed,
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Kind::File => 0,
            Kind::Folder => 1,
            Kind::Smaller => 2,
            Kind::Closed => 3,
        }
    }

    fn from_code(code: u8) -> Kind {
        match code {
            1 => Kind::Folder,
            2 => Kind::Smaller,
            3 => Kind::Closed,
            _ => Kind::File,
        }
    }

    fn word(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Folder => "folder",
            Kind::Smaller => "smaller",
            Kind::Closed => "closed",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub kind: Kind,
    /// Bytes on disk, with everything below.
    pub size: u64,
    /// Files, with everything below (1 for a file).
    pub files: u64,
    pub on: Vec<(u8, u64)>,
    /// Where its contents start in [`Map::nodes`], and how many.
    pub first: u32,
    pub count: u32,
}

/// A node as a line of the map's file.
#[derive(Serialize, Deserialize)]
struct Line(String, u8, u64, u64, Vec<(u8, u64)>, u32, u32);

pub struct Map {
    pub meta: Meta,
    pub nodes: Vec<Node>,
}

impl Map {
    /// Writes the map: what it's of on the first line, then a node a line.
    pub fn save(&self, file: &Path) -> Result<(), String> {
        let say = |e: std::io::Error| format!("{}: {e}", file.display());
        let temporary = file.with_extension("tmp");
        let mut out = BufWriter::new(std::fs::File::create(&temporary).map_err(say)?);
        let write = |out: &mut BufWriter<std::fs::File>| -> std::io::Result<()> {
            serde_json::to_writer(&mut *out, &self.meta)?;
            out.write_all(b"\n")?;
            for n in &self.nodes {
                let line = Line(
                    n.name.clone(),
                    n.kind.code(),
                    n.size,
                    n.files,
                    n.on.clone(),
                    n.first,
                    n.count,
                );
                serde_json::to_writer(&mut *out, &line)?;
                out.write_all(b"\n")?;
            }
            out.flush()
        };
        write(&mut out)
            .and_then(|()| std::fs::rename(&temporary, file))
            .map_err(|e| {
                let _ = std::fs::remove_file(&temporary);
                say(e)
            })
    }

    pub fn load(file: &Path) -> Result<Map, String> {
        let gone = "the map isn't there any more; build it again".to_string();
        let mut lines =
            BufReader::new(std::fs::File::open(file).map_err(|_| gone.clone())?).lines();
        let meta: Meta = lines
            .next()
            .and_then(|l| l.ok())
            .and_then(|l| serde_json::from_str(&l).ok())
            .ok_or_else(|| gone.clone())?;
        let mut nodes = Vec::new();
        for line in lines {
            let Line(name, kind, size, files, on, first, count) = line
                .ok()
                .and_then(|l| serde_json::from_str(&l).ok())
                .ok_or_else(|| gone.clone())?;
            nodes.push(Node {
                name,
                kind: Kind::from_code(kind),
                size,
                files,
                on,
                first,
                count,
            });
        }
        let total = nodes.len() as u64;
        if nodes.is_empty()
            || nodes
                .iter()
                .any(|n| u64::from(n.first) + u64::from(n.count) > total)
        {
            return Err(gone);
        }
        Ok(Map { meta, nodes })
    }

    fn children(&self, id: usize) -> std::ops::Range<usize> {
        let node = &self.nodes[id];
        node.first as usize..(node.first + node.count) as usize
    }

    /// The folder at a path below the map's top ("" for the top), names
    /// joined with "/".
    pub fn find(&self, path: &str) -> Option<usize> {
        let mut at = 0;
        for name in path.split('/').filter(|p| !p.is_empty()) {
            at = self
                .children(at)
                .find(|&c| self.nodes[c].kind == Kind::Folder && self.nodes[c].name == name)?;
        }
        Some(at)
    }

    fn disk_names(&self, on: &[(u8, u64)]) -> Vec<(String, u64)> {
        let mut disks: Vec<(String, u64)> = on
            .iter()
            .map(|&(d, bytes)| {
                let name = self.meta.disks.get(usize::from(d)).cloned();
                (name.unwrap_or_default(), bytes)
            })
            .collect();
        disks.sort_by_key(|d| std::cmp::Reverse(d.1));
        disks
    }

    fn item(&self, id: usize, path: &str, shape: Shape, depth: u8, group: u16) -> Item {
        let node = &self.nodes[id];
        Item {
            shape,
            name: node.name.clone(),
            path: match node.kind {
                Kind::Smaller => path.to_string(),
                _ => join(path, &node.name),
            },
            kind: node.kind.word(),
            size: node.size,
            files: node.files,
            depth,
            group,
            nested: false,
            disks: self.disk_names(&node.on),
        }
    }

    /// What to draw for the folder at `path` in a `width` × `height` view.
    pub fn view(&self, path: &str, style: Style, width: f32, height: f32) -> Result<View, String> {
        let path = path.trim_matches('/');
        let focus = self
            .find(path)
            .ok_or_else(|| "that folder isn't on the map".to_string())?;
        let width = width.clamp(120.0, 8000.0);
        let height = height.clamp(120.0, 8000.0);
        let mut items = Vec::new();
        match style {
            Style::Treemap => {
                let bounds = Rect::new(0.0, 0.0, width, height);
                self.treemap(focus, path, bounds, 0, None, &mut items);
                // A folder holding one thing would come out in one colour.
                if items.iter().all(|i| i.group == 0) && items.iter().any(|i| i.depth == 1) {
                    let mut next = 0u16;
                    let mut current = 0u16;
                    for item in &mut items {
                        if item.depth == 1 {
                            current = next;
                            next = next.saturating_add(1);
                        }
                        item.group = if item.depth == 0 { 0 } else { current };
                    }
                }
            }
            Style::Bubbles => self.bubbles(focus, path, width, height, &mut items),
        }
        let node = &self.nodes[focus];
        let mut crumbs = vec![Crumb {
            name: String::new(),
            path: String::new(),
        }];
        for name in path.split('/').filter(|p| !p.is_empty()) {
            let so_far = join(&crumbs[crumbs.len() - 1].path, name);
            crumbs.push(Crumb {
                name: name.to_string(),
                path: so_far,
            });
        }
        Ok(View {
            path: path.to_string(),
            size: node.size,
            files: node.files,
            disks: self.disk_names(&node.on),
            crumbs,
            items,
        })
    }

    fn treemap(
        &self,
        dir: usize,
        path: &str,
        bounds: Rect,
        depth: u8,
        group: Option<u16>,
        out: &mut Vec<Item>,
    ) {
        const MAX_DEPTH: u8 = 3;
        const MIN_SIDE: f32 = 3.0;
        const HEADER: f32 = 18.0;
        const PADDING: f32 = 2.0;
        const MAX_ITEMS: usize = 1500;

        // Biggest first already.
        let children: Vec<usize> = self
            .children(dir)
            .take_while(|&c| self.nodes[c].size > 0)
            .collect();
        let sizes: Vec<f64> = children
            .iter()
            .map(|&c| self.nodes[c].size as f64)
            .collect();
        let rects = treemap::squarify(&sizes, bounds);
        for (i, (&child, rect)) in children.iter().zip(rects).enumerate() {
            if out.len() >= MAX_ITEMS {
                return;
            }
            if rect.w < MIN_SIDE || rect.h < MIN_SIDE {
                continue;
            }
            let group = group.unwrap_or(i.min(usize::from(u16::MAX)) as u16);
            let node = &self.nodes[child];
            let inner = Rect::new(
                rect.x + PADDING,
                rect.y + HEADER,
                rect.w - 2.0 * PADDING,
                rect.h - HEADER - PADDING,
            );
            let nested = node.kind == Kind::Folder
                && node.count > 0
                && depth + 1 < MAX_DEPTH
                && inner.w >= 4.0 * MIN_SIDE
                && inner.h >= 4.0 * MIN_SIDE;
            let shape = Shape {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
                r: 0.0,
            };
            let mut item = self.item(child, path, shape, depth, group);
            item.nested = nested;
            let below = item.path.clone();
            out.push(item);
            if nested {
                self.treemap(child, &below, inner, depth + 1, Some(group), out);
            }
        }
    }

    /// A folder's biggest things, and the rest's bytes and count.
    fn biggest(&self, dir: usize, max: usize) -> (Vec<usize>, u64, u64) {
        const MIN_SHARE: f64 = 0.004;
        let children: Vec<usize> = self
            .children(dir)
            .take_while(|&c| self.nodes[c].size > 0)
            .collect();
        let total: u64 = children.iter().map(|&c| self.nodes[c].size).sum();
        let mut keep = children
            .iter()
            .take(max.max(1))
            .take_while(|&&c| self.nodes[c].size as f64 >= total as f64 * MIN_SHARE)
            .count()
            .max(children.len().min(1));
        // One leftover is drawn as itself.
        if children.len() == keep + 1 {
            keep += 1;
        }
        let rest = &children[keep..];
        let rest_size = rest.iter().map(|&c| self.nodes[c].size).sum();
        (children[..keep].to_vec(), rest_size, rest.len() as u64)
    }

    fn bubbles(&self, focus: usize, path: &str, width: f32, height: f32, out: &mut Vec<Item>) {
        const MAX_ITEMS: usize = 40;
        const MAX_NESTED: usize = 16;
        const NESTED_MIN_RADIUS: f32 = 44.0;

        let other = |circle: Circle, size: u64, count: u64, depth: u8, group: u16, at: &str| Item {
            shape: Shape::circle(circle),
            name: String::new(),
            path: at.to_string(),
            kind: "other",
            size,
            files: count,
            depth,
            group,
            nested: false,
            disks: Vec::new(),
        };
        let (top, rest, rest_count) = self.biggest(focus, MAX_ITEMS);
        let mut sizes: Vec<u64> = top.iter().map(|&c| self.nodes[c].size).collect();
        if rest > 0 {
            sizes.push(rest);
        }
        let circles = bubbles::fit(&sizes, width, height, 6.0);
        for (group, (&child, &circle)) in top.iter().zip(&circles).enumerate() {
            let group = group as u16;
            let mut item = self.item(child, path, Shape::circle(circle), 0, group);
            let below = item.path.clone();
            let node = &self.nodes[child];
            let (inner, inner_rest, inner_count) = if node.kind == Kind::Folder {
                self.biggest(child, MAX_NESTED)
            } else {
                (Vec::new(), 0, 0)
            };
            let mut inner_sizes: Vec<u64> = inner.iter().map(|&c| self.nodes[c].size).collect();
            if inner_rest > 0 {
                inner_sizes.push(inner_rest);
            }
            // A folder holding one thing would just draw a second circle.
            let nested = circle.r >= NESTED_MIN_RADIUS && inner_sizes.len() >= 2;
            item.nested = nested;
            out.push(item);
            if !nested {
                continue;
            }
            let placed = bubbles::fit_circle(&inner_sizes, circle, 0.12);
            for (&grandchild, &c) in inner.iter().zip(&placed) {
                out.push(self.item(grandchild, &below, Shape::circle(c), 1, group));
            }
            if inner_rest > 0
                && let Some(&c) = placed.get(inner.len())
            {
                out.push(other(c, inner_rest, inner_count, 1, group, &below));
            }
        }
        if rest > 0
            && let Some(&c) = circles.get(top.len())
        {
            out.push(other(c, rest, rest_count, 0, top.len() as u16, path));
        }
    }
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}/{name}")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Treemap,
    Bubbles,
}

impl Style {
    pub fn parse(text: &str) -> Result<Style, String> {
        match text {
            "treemap" => Ok(Style::Treemap),
            "bubbles" => Ok(Style::Bubbles),
            _ => Err("choose treemap or bubbles".into()),
        }
    }
}

/// A box (`x`, `y` its corner, `w` × `h`) or a bubble (`x`, `y` its
/// centre, `r`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct Shape {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub r: f32,
}

impl Shape {
    fn circle(c: Circle) -> Shape {
        Shape {
            x: c.x,
            y: c.y,
            w: 0.0,
            h: 0.0,
            r: c.r,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Item {
    #[serde(flatten)]
    pub shape: Shape,
    /// Empty for a folder's smaller files and a bubble of smaller things.
    pub name: String,
    /// Below the map's top; the folder they're in for smaller files and
    /// smaller things.
    pub path: String,
    /// `file`, `folder`, `smaller` (a folder's files without boxes of
    /// their own), `closed` (a folder that doesn't open) or `other` (the
    /// things too small for bubbles of their own).
    pub kind: &'static str,
    pub size: u64,
    /// Files inside; for `other`, how many things it stands for.
    pub files: u64,
    /// 0 for what's in the folder shown, 1 for what's inside those, …
    pub depth: u8,
    /// Which of the shown folder's things it belongs to, for colouring.
    pub group: u16,
    /// A folder with its contents drawn inside it.
    pub nested: bool,
    /// The disks it's on, most first.
    pub disks: Vec<(String, u64)>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Crumb {
    pub name: String,
    pub path: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct View {
    pub path: String,
    pub size: u64,
    pub files: u64,
    pub disks: Vec<(String, u64)>,
    /// The way down from the map's top (first, with no name) to here.
    pub crumbs: Vec<Crumb>,
    pub items: Vec<Item>,
}

// ----- Scanning ----------------------------------------------------------

/// Where the map's file goes: on a pool that's awake, in [`MAP_DIR`],
/// so it isn't held in memory (Unraid's own folders are); `fallback`
/// without one.
pub fn file_for(system: &System, array: &Array, fallback: &Path) -> PathBuf {
    array
        .roots(false)
        .iter()
        .filter(|d| d.kind == DiskKind::Pool)
        .filter_map(|d| d.path.as_deref())
        .map(|mount| system.file(mount).join(MAP_DIR))
        .find(|dir| std::fs::create_dir_all(dir).is_ok())
        .unwrap_or_else(|| fallback.to_path_buf())
        .join(MAP_FILE)
}

/// Scans what `target` covers, a disk at a time, and merges it. Sleeping
/// disks are spun up: the map was asked for. `progress` hears how far it
/// is; setting `cancel` stops it.
pub fn build(
    system: &System,
    array: &Array,
    target: &Target,
    limits: Limits,
    now: i64,
    progress: &dyn Fn(&str),
    cancel: &AtomicBool,
) -> Result<Map, String> {
    if !array.started {
        return Err("the array is stopped".into());
    }
    let started = std::time::Instant::now();
    let roots = array.roots(true);
    // Each disk with the folder on it to scan.
    let mut places: Vec<(String, PathBuf, bool)> = Vec::new();
    let mut missed = Vec::new();
    let subvolumes = |fs: &str| matches!(fs, "zfs" | "btrfs");
    match target {
        Target::All => {
            for disk in &roots {
                let mount = disk.path.as_deref().unwrap_or_default();
                places.push((disk.name.clone(), system.file(mount), subvolumes(&disk.fs)));
            }
        }
        Target::Disk(name) => {
            let disk = roots
                .iter()
                .find(|d| &d.name == name)
                .ok_or_else(|| format!("{name} isn't a mounted disk or pool"))?;
            let mount = disk.path.as_deref().unwrap_or_default();
            places.push((disk.name.clone(), system.file(mount), subvolumes(&disk.fs)));
        }
        Target::Share(share) => {
            for disk in &roots {
                let mount = disk.path.as_deref().unwrap_or_default();
                let folder = system.file(mount).join(share);
                if folder.is_dir() {
                    places.push((disk.name.clone(), folder, subvolumes(&disk.fs)));
                }
            }
            if places.is_empty() {
                return Err(format!("no disk has a {share} share"));
            }
        }
    }
    if places.is_empty() {
        return Err("no disk or pool is mounted".into());
    }
    for disk in &array.disks {
        if matches!(disk.kind, DiskKind::Data | DiskKind::Pool)
            && disk.path.is_none()
            && match target {
                Target::Disk(_) => false,
                Target::All | Target::Share(_) => true,
            }
        {
            missed.push(format!("{}: not mounted", disk.name));
        }
    }

    let mut builder = Builder::new(limits);
    let mut unreadable = 0;
    let mut scanned = 0;
    let total = places.len();
    for (i, (name, folder, subvolumes)) in places.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        let what = if total == 1 {
            format!("reading {name}")
        } else {
            format!("reading {name} ({} of {total})", i + 1)
        };
        progress(&what);
        // ZFS datasets and btrfs subvolumes are filesystems of their own,
        // inside the pool.
        let options = ScanOptions {
            one_filesystem: !subvolumes,
            ..ScanOptions::default()
        };
        let watching = ScanProgress::new();
        let result = std::thread::scope(|scope| {
            let running = scope.spawn(|| scan(&folder, &options, &watching));
            let mut ticks = 0u32;
            while !running.is_finished() {
                std::thread::sleep(Duration::from_millis(100));
                if cancel.load(Ordering::Relaxed) {
                    watching.cancel();
                }
                ticks += 1;
                if ticks.is_multiple_of(20) {
                    let files = watching.snapshot().files;
                    progress(&format!("{what}: {} files so far", format_count(files)));
                }
            }
            running.join()
        });
        if cancel.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        match result {
            Ok(Ok(found)) => {
                unreadable += found.errors;
                builder.add_disk(&name, &found.tree, found.tree.root());
                scanned += 1;
            }
            Ok(Err(err)) => missed.push(format!("{name}: {err}")),
            Err(_) => missed.push(format!("{name}: the scan stopped unexpectedly")),
        }
    }
    if scanned == 0 {
        return Err(missed.join("; "));
    }
    progress("putting the map together");
    let meta = Meta {
        target: target.code(),
        root: match target {
            Target::All => "/mnt/user".into(),
            Target::Disk(name) => array
                .disk(name)
                .and_then(|d| d.path.clone())
                .unwrap_or_default(),
            Target::Share(share) => format!("/mnt/user/{share}"),
        },
        built: now,
        missed,
        unreadable,
        ..Meta::default()
    };
    let mut map = builder.finish(meta);
    map.meta.seconds = started.elapsed().as_secs();
    map.meta.peak_memory = crate::peak_memory();
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::array::{self, tests::server};

    fn write(system: &System, files: &[(&str, usize)]) {
        for (path, bytes) in files {
            let file = system.file(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, vec![7u8; *bytes]).unwrap();
        }
    }

    fn built(system: &System, target: &str, limits: Limits) -> Map {
        let array = array::read(system);
        build(
            system,
            &array,
            &Target::parse(target).unwrap(),
            limits,
            100,
            &|_| {},
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn names(map: &Map, path: &str) -> Vec<String> {
        let at = map.find(path).unwrap();
        map.children(at)
            .map(|c| {
                let n = &map.nodes[c];
                match n.kind {
                    Kind::Smaller => format!("({} smaller)", n.files),
                    Kind::Closed => format!("[{}]", n.name),
                    _ => n.name.clone(),
                }
            })
            .collect()
    }

    const FILES: &[(&str, usize)] = &[
        ("/mnt/disk1/Movies/Alien (1979)/alien.mkv", 400_000),
        ("/mnt/disk1/Movies/Brazil (1985)/brazil.mkv", 300_000),
        ("/mnt/disk1/Movies/Brazil (1985)/brazil.srt", 4_000),
        // The same folder on a sleeping disk, and on the pool.
        ("/mnt/disk2/Movies/Alien (1979)/extras.mkv", 200_000),
        ("/mnt/disk2/Music/a.flac", 50_000),
        ("/mnt/cache/Movies/new.mkv", 100_000),
        ("/mnt/cache/appdata/plex/db", 30_000),
    ];

    #[test]
    fn targets() {
        assert_eq!(Target::parse("all"), Ok(Target::All));
        assert_eq!(
            Target::parse("disk:disk1"),
            Ok(Target::Disk("disk1".into()))
        );
        assert_eq!(
            Target::parse("share:My Films"),
            Ok(Target::Share("My Films".into()))
        );
        for bad in ["", "disk1", "share:", "share:a/b", "share:..", "folder:x"] {
            assert!(Target::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Target::parse("share:TV").unwrap().code(), "share:TV");
    }

    #[test]
    fn all_merges_disks_by_path_and_counts_once() {
        let (_dir, system) = server();
        write(&system, FILES);
        let map = built(&system, "all", Limits::default());
        assert_eq!(map.meta.root, "/mnt/user");
        // Sleeping disks are read too: the map was asked for.
        assert_eq!(map.meta.disks, ["disk1", "disk2", "cache"]);
        assert_eq!(map.meta.files, 7);
        // One Movies, not one per disk; biggest first.
        assert_eq!(names(&map, ""), ["Movies", "Music", "appdata"]);
        assert_eq!(
            names(&map, "Movies"),
            ["Alien (1979)", "Brazil (1985)", "new.mkv"]
        );
        assert_eq!(
            names(&map, "Movies/Alien (1979)"),
            ["alien.mkv", "extras.mkv"]
        );
        // Sizes are the disks' added together, nothing twice.
        let size = |path: &str| map.nodes[map.find(path).unwrap()].size;
        assert!(size("Movies") >= 1_004_000 && size("Movies") < 1_100_000);
        assert_eq!(size(""), size("Movies") + size("Music") + size("appdata"));
        let alien = &map.nodes[map.find("Movies/Alien (1979)").unwrap()];
        assert_eq!(alien.files, 2);
        let on = map.disk_names(&alien.on);
        assert_eq!(on.len(), 2);
        assert_eq!(on[0].0, "disk1");
        assert_eq!(on[1].0, "disk2");
        assert_eq!(on[0].1 + on[1].1, alien.size);
    }

    #[test]
    fn one_disk_and_one_share() {
        let (_dir, system) = server();
        write(&system, FILES);
        let disk = built(&system, "disk:disk2", Limits::default());
        assert_eq!(disk.meta.root, "/mnt/disk2");
        assert_eq!(disk.meta.disks, ["disk2"]);
        assert_eq!(names(&disk, ""), ["Movies", "Music"]);

        let share = built(&system, "share:Movies", Limits::default());
        assert_eq!(share.meta.root, "/mnt/user/Movies");
        assert_eq!(share.meta.files, 5);
        assert_eq!(
            names(&share, ""),
            ["Alien (1979)", "Brazil (1985)", "new.mkv"]
        );

        let array = array::read(&system);
        let fails = |target: &str| {
            build(
                &system,
                &array,
                &Target::parse(target).unwrap(),
                Limits::default(),
                0,
                &|_| {},
                &AtomicBool::new(false),
            )
            .err()
            .unwrap()
        };
        assert_eq!(fails("share:Nothing"), "no disk has a Nothing share");
        assert_eq!(fails("disk:parity"), "parity isn't a mounted disk or pool");
    }

    #[test]
    fn small_files_are_summed_past_the_limit() {
        let (_dir, system) = server();
        write(&system, FILES);
        let limits = Limits {
            files: 4,
            folders: 100,
        };
        let map = built(&system, "all", limits);
        // Nothing is lost: the same totals as with every file kept.
        let whole = built(&system, "all", Limits::default());
        assert_eq!(map.meta.size, whole.meta.size);
        assert_eq!(map.meta.files, 7);
        assert!(map.meta.floor > 0);
        let boxes = map.nodes.iter().filter(|n| n.kind == Kind::File).count();
        assert!((1..=4).contains(&boxes), "{boxes}");
        // The biggest keep their boxes; the subtitle doesn't.
        assert_eq!(
            names(&map, "Movies/Alien (1979)"),
            ["alien.mkv", "extras.mkv"]
        );
        assert_eq!(
            names(&map, "Movies/Brazil (1985)"),
            ["brazil.mkv", "(1 smaller)"]
        );
    }

    #[test]
    fn small_folders_are_closed_past_the_limit() {
        let (_dir, system) = server();
        write(&system, FILES);
        let limits = Limits {
            files: 100,
            folders: 4,
        };
        let map = built(&system, "all", limits);
        let whole = built(&system, "all", Limits::default());
        assert_eq!(map.meta.size, whole.meta.size);
        assert_eq!(map.meta.files, 7);
        assert!(map.meta.folder_floor > 0);
        assert_eq!(names(&map, ""), ["Movies", "[Music]", "[appdata]"]);
        // A closed folder still says what's in it and where.
        let top = map.find("").unwrap();
        let appdata = map
            .children(top)
            .map(|c| &map.nodes[c])
            .find(|n| n.name == "appdata")
            .unwrap();
        assert_eq!(appdata.files, 1);
        assert_eq!(map.disk_names(&appdata.on)[0].0, "cache");
        assert_eq!(map.find("appdata"), None);
    }

    #[test]
    fn saves_and_loads() {
        let (dir, system) = server();
        write(&system, FILES);
        write(&system, &[("/mnt/disk1/Movies/odd \"name\"\n.mkv", 9_000)]);
        let map = built(&system, "all", Limits::default());
        let file = dir.path().join("map.dat");
        map.save(&file).unwrap();
        let loaded = Map::load(&file).unwrap();
        assert_eq!(loaded.meta, map.meta);
        assert_eq!(loaded.nodes, map.nodes);
        std::fs::write(&file, "nonsense").unwrap();
        assert!(Map::load(&file).is_err());
        assert!(Map::load(&dir.path().join("none.dat")).is_err());
    }

    #[test]
    fn the_file_goes_on_a_pool() {
        let (dir, system) = server();
        let array = array::read(&system);
        std::fs::create_dir_all(system.file("/mnt/cache")).unwrap();
        assert_eq!(
            file_for(&system, &array, dir.path()),
            system.file("/mnt/cache/.freesize/map.dat")
        );
    }

    #[test]
    fn treemap_view() {
        let (_dir, system) = server();
        write(&system, FILES);
        let map = built(&system, "all", Limits::default());
        let view = map.view("", Style::Treemap, 800.0, 600.0).unwrap();
        assert_eq!(view.size, map.meta.size);
        assert_eq!(view.crumbs.len(), 1);
        let top: Vec<&Item> = view.items.iter().filter(|i| i.depth == 0).collect();
        assert_eq!(top.len(), 3);
        assert_eq!(top[0].name, "Movies");
        assert_eq!(top[0].path, "Movies");
        assert!(top[0].nested);
        // The boxes fill the view in proportion.
        let area: f32 = top.iter().map(|i| i.shape.w * i.shape.h).sum();
        assert!((area - 480_000.0).abs() < 500.0, "{area}");
        let share = top[0].shape.w * top[0].shape.h / area;
        let expected = top[0].size as f32 / view.size as f32;
        assert!((share - expected).abs() < 0.01);
        // What's inside is drawn inside, in its folder's colour.
        let alien = view
            .items
            .iter()
            .find(|i| i.path == "Movies/Alien (1979)")
            .unwrap();
        assert_eq!((alien.depth, alien.group, alien.kind), (1, 0, "folder"));
        assert!(alien.shape.x >= top[0].shape.x && alien.shape.y >= top[0].shape.y + 18.0);
        assert_eq!(alien.disks.len(), 2);

        let deeper = map.view("/Movies/Alien (1979)/", Style::Treemap, 800.0, 600.0);
        let deeper = deeper.unwrap();
        assert_eq!(deeper.path, "Movies/Alien (1979)");
        assert_eq!(
            deeper
                .crumbs
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            ["", "Movies", "Movies/Alien (1979)"]
        );
        assert_eq!(deeper.items.len(), 2);
        assert_eq!(deeper.items[0].kind, "file");
        assert!(map.view("Nope", Style::Treemap, 800.0, 600.0).is_err());
        // A file isn't a folder to open.
        assert!(
            map.view("Movies/new.mkv", Style::Treemap, 800.0, 600.0)
                .is_err()
        );
    }

    #[test]
    fn bubbles_view() {
        let (_dir, system) = server();
        write(&system, FILES);
        let map = built(&system, "all", Limits::default());
        let view = map.view("", Style::Bubbles, 800.0, 600.0).unwrap();
        let top: Vec<&Item> = view.items.iter().filter(|i| i.depth == 0).collect();
        assert_eq!(top.len(), 3);
        for item in &view.items {
            let s = item.shape;
            assert!(s.r > 0.0);
            assert!(s.x - s.r >= 0.0 && s.x + s.r <= 800.0, "{item:?}");
            assert!(s.y - s.r >= 0.0 && s.y + s.r <= 600.0, "{item:?}");
        }
        // Movies shows what's in it, inside its own bubble.
        assert!(top[0].nested);
        let inside: Vec<&Item> = view.items.iter().filter(|i| i.depth == 1).collect();
        assert_eq!(inside.len(), 3);
        for item in inside {
            let reach = (item.shape.x - top[0].shape.x).hypot(item.shape.y - top[0].shape.y);
            assert!(reach + item.shape.r <= top[0].shape.r);
        }
    }

    #[test]
    fn many_small_things_share_a_bubble() {
        let (_dir, system) = server();
        let mut files: Vec<(String, usize)> = (0..300)
            .map(|i| (format!("/mnt/disk1/Lots/f{i:03}"), 4_096))
            .collect();
        files.push(("/mnt/disk1/Lots/big".into(), 4_096_000));
        for (path, bytes) in &files {
            write(&system, &[(path.as_str(), *bytes)]);
        }
        let map = built(&system, "disk:disk1", Limits::default());
        let view = map.view("Lots", Style::Bubbles, 600.0, 600.0).unwrap();
        let other = view.items.iter().find(|i| i.kind == "other").unwrap();
        assert!(other.files > 200, "{}", other.files);
        assert_eq!(other.path, "Lots");
        let shown: u64 = view.items.iter().map(|i| i.size).sum();
        assert_eq!(shown, view.size);
    }

    #[test]
    fn stops_when_asked() {
        let (_dir, system) = server();
        write(&system, FILES);
        let array = array::read(&system);
        let stopped = build(
            &system,
            &array,
            &Target::All,
            Limits::default(),
            0,
            &|_| {},
            &AtomicBool::new(true),
        );
        assert_eq!(stopped.err().unwrap(), "stopped");
    }

    #[test]
    fn floors() {
        let mut sizes = vec![5, 9, 1, 7, 3, 8, 2, 6];
        assert_eq!(floor_for(&mut sizes.clone(), 8), None);
        // Room for 4: the 3 biggest stay.
        assert_eq!(floor_for(&mut sizes, 4), Some(7));
    }
}
