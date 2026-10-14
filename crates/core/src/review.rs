//! Reviewing a folder before deleting it, such as an old backup of a PC:
//! its files by what they are (photos, documents, videos, the rest), the
//! ones worth a look first (documents, password vaults, keys, wallets, tax
//! files, email, camera photos), and which of them are kept elsewhere in
//! the scan too, by content.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use crate::kinds::Kind;
use crate::reclaim::{DuplicateProgress, full_hash, sample_hash};
use crate::tree::{NodeId, NodeKind, SizeMode, Tree, flags};

/// What a file is, for the review's tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    Photos,
    Documents,
    Videos,
    Other,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Photos, Group::Documents, Group::Videos, Group::Other];

    pub fn key(self) -> &'static str {
        match self {
            Group::Photos => "photos",
            Group::Documents => "documents",
            Group::Videos => "videos",
            Group::Other => "other",
        }
    }
}

/// The file's extension, lower case, without the dot.
fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default()
}

/// The group of a file called `name`.
pub fn group_of(name: &str) -> Group {
    match Kind::of_extension(extension(name).as_bytes()) {
        Kind::Image => Group::Photos,
        Kind::Document => Group::Documents,
        Kind::Video => Group::Videos,
        _ => Group::Other,
    }
}

/// Why a file is worth a look before its folder is deleted, if it is:
/// `name` is the file's name, `path` its path (any separator).
pub fn important(name: &str, path: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let ext = extension(&lower);
    let path = path.to_ascii_lowercase().replace('\\', "/");
    match ext.as_str() {
        "kdbx" | "kdb" | "opvault" | "agilekeychain" | "1pif" | "1pux" | "psafe3" | "keychain"
        | "keychain-db" => return Some("Password vault"),
        "pem" | "key" | "p12" | "pfx" | "ppk" | "asc" | "gpg" | "pgp" | "crt" | "cer" | "der"
        | "jks" | "keystore" | "ovpn" | "kdbx-key" => return Some("Key or certificate"),
        "wallet" => return Some("Crypto wallet"),
        "pst" | "ost" | "mbox" | "eml" | "msg" | "olm" | "emlx" => return Some("Email"),
        "qbw" | "qbb" | "qbm" | "qdf" | "qif" | "ofx" | "qfx" | "gnucash" | "mny" | "tax" => {
            return Some("Tax or accounting");
        }
        _ => {}
    }
    // TurboTax and similar keep the year in the extension: .tax2019.
    if ext.starts_with("tax") && ext[3..].chars().all(|c| c.is_ascii_digit()) && ext.len() > 3 {
        return Some("Tax or accounting");
    }
    if matches!(lower.as_str(), "wallet.dat" | "default_wallet") || lower.starts_with("utc--") {
        return Some("Crypto wallet");
    }
    if matches!(
        lower.as_str(),
        "id_rsa" | "id_ed25519" | "id_ecdsa" | "id_dsa" | "authorized_keys" | "known_hosts"
    ) || (lower.starts_with("bitwarden_export") && ext == "json")
    {
        return if lower.starts_with("bitwarden") {
            Some("Password vault")
        } else {
            Some("Key or certificate")
        };
    }
    match group_of(&lower) {
        Group::Documents => Some(
            if matches!(ext.as_str(), "xls" | "xlsx" | "ods" | "numbers" | "csv") {
                "Spreadsheet"
            } else {
                "Document"
            },
        ),
        Group::Photos if camera_photo(&lower, &path) => Some("Camera photo"),
        _ => None,
    }
}

/// A photo a camera or phone took, by its name (IMG_1234, DSC01234,
/// PXL_20240101_…, 20240101_123456) or a DCIM folder.
fn camera_photo(lower: &str, path: &str) -> bool {
    let stem = lower.rsplit_once('.').map_or(lower, |(s, _)| s);
    let digits_after = |prefix: &str| {
        stem.strip_prefix(prefix)
            .is_some_and(|rest| rest.chars().take(4).filter(char::is_ascii_digit).count() == 4)
    };
    path.contains("/dcim/")
        || [
            "img_", "img", "dsc_", "dsc", "dscn", "dscf", "pxl_", "p", "gopr", "dji_", "_mg_",
        ]
        .iter()
        .any(|p| digits_after(p))
        || (stem.len() >= 15
            && stem[..8].chars().all(|c| c.is_ascii_digit())
            && stem.as_bytes()[8] == b'_')
}

/// Every file below `folder`, biggest first.
pub fn files(tree: &Tree, folder: NodeId, mode: SizeMode) -> Vec<NodeId> {
    let mut files: Vec<NodeId> = tree
        .descendants(folder)
        .filter(|&id| tree.node(id).kind == NodeKind::File && !tree.node(id).has(flags::DELETED))
        .collect();
    files.sort_by(|&a, &b| {
        tree.node(b)
            .size_in(mode)
            .cmp(&tree.node(a).size_in(mode))
            .then(a.cmp(&b))
    });
    files
}

/// A file of the folder (its node, path and size) and the files outside it
/// of its size.
type Pair = (NodeId, PathBuf, u64, Vec<(NodeId, PathBuf)>);

/// The files of a folder and the files elsewhere in the scan of the same
/// size: the input of [`CopyCandidates::find`]. Owns its paths, so the
/// reading can run on another thread.
#[derive(Clone, Debug, Default)]
pub struct CopyCandidates {
    /// A file in the folder, then the files outside it of its size.
    pairs: Vec<Pair>,
}

/// Pairs each file below `folder` with the files of the same size in the
/// rest of `tree`. Cloud placeholders and empty files are left out.
pub fn copy_candidates(tree: &Tree, folder: NodeId) -> CopyCandidates {
    let skip = flags::CLOUD | flags::ERROR | flags::DELETED;
    let usable = |id: NodeId| {
        let node = tree.node(id);
        node.kind == NodeKind::File && node.flags & skip == 0 && node.size > 0
    };
    let inside: Vec<NodeId> = tree.descendants(folder).filter(|&id| usable(id)).collect();
    let sizes: std::collections::HashSet<u64> =
        inside.iter().map(|&id| tree.node(id).size).collect();
    let mut outside: HashMap<u64, Vec<NodeId>> = HashMap::new();
    let root = tree.root();
    for id in tree.descendants(root) {
        if usable(id)
            && sizes.contains(&tree.node(id).size)
            && !tree.ancestors(id).any(|a| a == folder)
        {
            outside.entry(tree.node(id).size).or_default().push(id);
        }
    }
    let pairs = inside
        .into_iter()
        .filter_map(|id| {
            let size = tree.node(id).size;
            let others = outside.get(&size)?;
            Some((
                id,
                tree.path(id),
                size,
                others.iter().map(|&o| (o, tree.path(o))).collect(),
            ))
        })
        .collect();
    CopyCandidates { pairs }
}

impl CopyCandidates {
    /// How many of the folder's files have a file of their size elsewhere.
    pub fn files(&self) -> usize {
        self.pairs.len()
    }

    /// For each file of the folder kept elsewhere too, one of the other
    /// copies. Compares a hash of both ends first, then the whole file.
    /// Cancelling returns what was confirmed so far.
    pub fn find(self, progress: &DuplicateProgress) -> HashMap<NodeId, NodeId> {
        progress
            .files
            .store(self.pairs.len() as u64, Ordering::Relaxed);
        let mut samples: HashMap<NodeId, Option<u128>> = HashMap::new();
        let mut wholes: HashMap<NodeId, Option<u128>> = HashMap::new();
        let mut found = HashMap::new();
        for (id, path, size, others) in self.pairs {
            if progress.is_cancelled() {
                break;
            }
            let Some(mine) = sample_hash(&path, size) else {
                progress.done.fetch_add(1, Ordering::Relaxed);
                continue;
            };
            let mut whole: Option<Option<u128>> = None;
            for (other, other_path) in others {
                let theirs = *samples
                    .entry(other)
                    .or_insert_with(|| sample_hash(&other_path, size));
                if theirs != Some(mine) {
                    continue;
                }
                let mine_whole = *whole.get_or_insert_with(|| full_hash(&path));
                let theirs_whole = *wholes
                    .entry(other)
                    .or_insert_with(|| full_hash(&other_path));
                if mine_whole.is_some() && mine_whole == theirs_whole {
                    found.insert(id, other);
                    break;
                }
            }
            progress.done.fetch_add(1, Ordering::Relaxed);
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::{ScanOptions, ScanProgress, scan};

    fn write(root: &Path, path: &str, data: &[u8]) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, data).unwrap();
    }

    #[test]
    fn sorts_and_flags_files() {
        assert_eq!(group_of("a.JPG"), Group::Photos);
        assert_eq!(group_of("a.docx"), Group::Documents);
        assert_eq!(group_of("a.mkv"), Group::Videos);
        assert_eq!(group_of("a.exe"), Group::Other);
        assert_eq!(
            important("vault.kdbx", "/b/vault.kdbx"),
            Some("Password vault")
        );
        assert_eq!(
            important("id_ed25519", "/b/.ssh/id_ed25519"),
            Some("Key or certificate")
        );
        assert_eq!(
            important("wallet.dat", "/b/wallet.dat"),
            Some("Crypto wallet")
        );
        assert_eq!(
            important("Return.tax2019", "/b/Return.tax2019"),
            Some("Tax or accounting")
        );
        assert_eq!(important("Outlook.pst", "/b/Outlook.pst"), Some("Email"));
        assert_eq!(
            important("budget.xlsx", "/b/budget.xlsx"),
            Some("Spreadsheet")
        );
        assert_eq!(important("letter.pdf", "/b/letter.pdf"), Some("Document"));
        assert_eq!(
            important("IMG_1234.JPG", "/b/IMG_1234.JPG"),
            Some("Camera photo")
        );
        assert_eq!(
            important("PXL_20240101_101010.jpg", "/x/p.jpg"),
            Some("Camera photo")
        );
        assert_eq!(
            important("20230405_121314.jpg", "/x/a.jpg"),
            Some("Camera photo")
        );
        assert_eq!(
            important("x.png", "C:\\Backup\\DCIM\\x.png"),
            Some("Camera photo")
        );
        assert_eq!(important("logo.png", "/b/logo.png"), None);
        assert_eq!(important("setup.exe", "/b/setup.exe"), None);
        assert_eq!(
            important("taxi.tax", "/b/taxi.tax"),
            Some("Tax or accounting")
        );
        assert_eq!(important("taxes.txt", "/b/taxes.txt"), Some("Document"));
    }

    #[test]
    fn finds_copies_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Same content as a file outside, same size but different content,
        // and only here.
        write(root, "Backup/a.jpg", &[1u8; 70_000]);
        write(root, "Pictures/2019/a-copy.jpg", &[1u8; 70_000]);
        let mut b = vec![2u8; 200_000];
        write(root, "Backup/b.jpg", &b);
        b[100_000] = 3;
        write(root, "Pictures/b.jpg", &b);
        write(root, "Backup/c.txt", b"only here");
        // A copy inside the folder itself doesn't count.
        write(root, "Backup/sub/c.txt", b"only here");
        let tree = scan(root, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let backup = crate::snapshot::find(&tree, "Backup").unwrap();
        assert_eq!(files(&tree, backup, SizeMode::Logical).len(), 4);
        let candidates = copy_candidates(&tree, backup);
        assert_eq!(candidates.files(), 2);
        let found = candidates.find(&DuplicateProgress::new());
        let a = crate::snapshot::find(&tree, "Backup/a.jpg").unwrap();
        let copy = crate::snapshot::find(&tree, "Pictures/2019/a-copy.jpg").unwrap();
        assert_eq!(found, HashMap::from([(a, copy)]));
    }
}
