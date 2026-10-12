//! What a file or folder mostly holds (videos, photos, games…), judged by
//! file extensions, so views can show a fitting icon.

use crate::tree::{NodeKind, Tree, flags};

/// The kind of content, as shown by an icon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Kind {
    /// A folder with no single kind making up most of it.
    #[default]
    Folder,
    /// A file of no particular kind.
    File,
    Video,
    Image,
    Audio,
    Archive,
    DiskImage,
    Document,
    Code,
    /// Programs and libraries: `.exe`, `.dll`, `.so`, `.apk`, installers.
    Program,
    /// Game data packages.
    Game,
    Database,
}

/// Kinds a folder can take on from its files.
const CONTENT: [Kind; 10] = [
    Kind::Video,
    Kind::Image,
    Kind::Audio,
    Kind::Archive,
    Kind::DiskImage,
    Kind::Document,
    Kind::Code,
    Kind::Program,
    Kind::Game,
    Kind::Database,
];

/// A folder takes on a kind when files of that kind are at least this share
/// of its bytes.
const DOMINANT: f64 = 0.5;

impl Kind {
    /// The kind of a file with this extension (any case, without the dot).
    pub fn of_extension(ext: &[u8]) -> Kind {
        let mut lower = [0u8; 8];
        if ext.len() > lower.len() {
            return Kind::File;
        }
        for (l, &b) in lower.iter_mut().zip(ext) {
            *l = b.to_ascii_lowercase();
        }
        match &lower[..ext.len()] {
            b"mp4" | b"mkv" | b"mov" | b"avi" | b"wmv" | b"webm" | b"m4v" | b"mpg" | b"mpeg"
            | b"3gp" | b"mts" | b"m2ts" | b"flv" | b"vob" | b"insv" | b"lrv" => Kind::Video,
            b"jpg" | b"jpeg" | b"png" | b"gif" | b"heic" | b"heif" | b"webp" | b"bmp" | b"tif"
            | b"tiff" | b"dng" | b"raw" | b"cr2" | b"cr3" | b"nef" | b"arw" | b"orf" | b"rw2"
            | b"psd" | b"avif" | b"insp" | b"xcf" | b"kra" => Kind::Image,
            b"mp3" | b"flac" | b"wav" | b"aac" | b"m4a" | b"ogg" | b"opus" | b"wma" | b"aiff"
            | b"aif" | b"alac" | b"mid" | b"midi" => Kind::Audio,
            b"zip" | b"rar" | b"7z" | b"tar" | b"gz" | b"tgz" | b"bz2" | b"xz" | b"zst"
            | b"lz4" | b"cab" | b"lzma" => Kind::Archive,
            b"iso" | b"img" | b"dmg" | b"vhd" | b"vhdx" | b"vmdk" | b"qcow2" | b"vdi" | b"wim"
            | b"esd" => Kind::DiskImage,
            b"pdf" | b"doc" | b"docx" | b"xls" | b"xlsx" | b"ppt" | b"pptx" | b"odt" | b"ods"
            | b"odp" | b"rtf" | b"txt" | b"epub" | b"md" | b"csv" | b"pages" | b"key"
            | b"numbers" => Kind::Document,
            b"rs" | b"c" | b"h" | b"cc" | b"cpp" | b"hpp" | b"py" | b"js" | b"mjs" | b"ts"
            | b"tsx" | b"jsx" | b"go" | b"java" | b"kt" | b"swift" | b"rb" | b"php" | b"cs"
            | b"lua" | b"sh" | b"html" | b"css" | b"scss" | b"vue" | b"dart" | b"o" | b"rlib"
            | b"rmeta" | b"class" | b"pyc" | b"map" => Kind::Code,
            b"exe" | b"dll" | b"so" | b"dylib" | b"apk" | b"apks" | b"xapk" | b"aab" | b"msi"
            | b"msix" | b"deb" | b"rpm" | b"appimage" | b"jar" | b"sys" | b"pkg" | b"snap"
            | b"flatpak" | b"odex" | b"vdex" | b"oat" => Kind::Program,
            b"pak" | b"bsa" | b"ba2" | b"uasset" | b"ubulk" | b"utoc" | b"ucas" | b"assets"
            | b"resource" | b"wad" | b"obb" | b"bundle" | b"forge" | b"rpf" | b"vpk" => Kind::Game,
            b"db" | b"sqlite" | b"sqlite3" | b"realm" | b"mdb" | b"accdb" | b"ldb" | b"wal" => {
                Kind::Database
            }
            _ => Kind::File,
        }
    }

    fn content_index(self) -> Option<usize> {
        CONTENT.iter().position(|&k| k == self)
    }
}

/// The kind of every node, indexed by node id. Files go by their extension;
/// a folder takes the kind making up at least half of its bytes, or stays a
/// plain folder.
pub fn kinds(tree: &Tree) -> Vec<Kind> {
    let nodes = tree.nodes();
    let mut out = vec![Kind::Folder; nodes.len()];
    // Bytes per content kind, for folders only.
    let mut slot = vec![u32::MAX; nodes.len()];
    let mut sums: Vec<[u64; CONTENT.len()]> = Vec::new();
    for (id, node) in nodes.iter().enumerate() {
        if node.kind == NodeKind::Dir {
            slot[id] = sums.len() as u32;
            sums.push([0; CONTENT.len()]);
        }
    }
    // Children come after their parent, so a reverse pass finishes every
    // folder before adding it to its own parent.
    for id in (0..nodes.len()).rev() {
        let node = &nodes[id];
        let parent = node.parent as usize;
        let has_parent = id != tree.root() as usize && parent < nodes.len();
        if node.kind == NodeKind::Dir {
            let own = sums[slot[id] as usize];
            let total = node.size.max(1) as f64;
            if let Some((i, &bytes)) = own.iter().enumerate().max_by_key(|&(_, b)| *b)
                && bytes as f64 >= total * DOMINANT
                && bytes > 0
            {
                out[id] = CONTENT[i];
            }
            if has_parent && slot[parent] != u32::MAX {
                let p = slot[parent] as usize;
                for (sum, add) in sums[p].iter_mut().zip(own) {
                    *sum += add;
                }
            }
        } else if node.kind == NodeKind::File {
            let kind = tree
                .extension(id as u32)
                .map_or(Kind::File, Kind::of_extension);
            out[id] = kind;
            if has_parent
                && !node.has(flags::HARDLINK_DUP)
                && slot[parent] != u32::MAX
                && let Some(i) = kind.content_index()
            {
                sums[slot[parent] as usize][i] += node.size;
            }
        } else {
            out[id] = Kind::File;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::{ScanOptions, ScanProgress, scan};

    #[test]
    fn extensions() {
        assert_eq!(Kind::of_extension(b"MP4"), Kind::Video);
        assert_eq!(Kind::of_extension(b"heic"), Kind::Image);
        assert_eq!(Kind::of_extension(b"vpk"), Kind::Game);
        assert_eq!(Kind::of_extension(b"iso"), Kind::DiskImage);
        assert_eq!(Kind::of_extension(b"weird"), Kind::File);
        assert_eq!(Kind::of_extension(b"averyverylongext"), Kind::File);
    }

    #[test]
    fn folders_take_the_kind_of_most_of_their_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, bytes: usize| {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, vec![0u8; bytes]).unwrap();
        };
        write("videos/a.mp4", 9000);
        write("videos/notes.txt", 100);
        write("mixed/a.mp4", 1000);
        write("mixed/b.zip", 1000);
        write("mixed/c.pdf", 1000);
        write("trip/day1/x.jpg", 5000);
        write("trip/day2/y.jpg", 5000);
        write("trip/clip.mp4", 2000);
        let tree = scan(root, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let kinds = kinds(&tree);
        let of = |name: &str| {
            let id = tree.child_by_name(tree.root(), name.as_ref()).unwrap();
            kinds[id as usize]
        };
        assert_eq!(of("videos"), Kind::Video);
        assert_eq!(of("mixed"), Kind::Folder);
        // Nested folders count toward their parent.
        assert_eq!(of("trip"), Kind::Image);
        let videos = tree.child_by_name(tree.root(), "videos".as_ref()).unwrap();
        let notes = tree.child_by_name(videos, "notes.txt".as_ref()).unwrap();
        assert_eq!(kinds[notes as usize], Kind::Document);
    }
}
