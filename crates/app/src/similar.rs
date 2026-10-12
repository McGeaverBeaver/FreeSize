//! Photos that look alike: bursts, retakes, and the same picture saved again
//! at another size or quality. Each photo gets a fingerprint of its
//! brightness pattern (difference hashes of a 9×9 grid, across and down);
//! photos whose fingerprints nearly match form a group, largest first.
//!
//! Most camera photos carry a small preview in their EXIF data. That is read
//! instead of the whole picture, so a phone's photos are fingerprinted in
//! seconds rather than minutes.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use disktree_core::{NodeId, Tree};
use image::{DynamicImage, ImageFormat, ImageReader, Limits};

use crate::owners::Pixels;

/// Photos smaller than this (icons, stickers, thumbnails) are skipped.
pub const MIN_PHOTO: u64 = 50 * 1024;
/// Files bigger than this aren't decoded.
const MAX_FILE: u64 = 64 * 1024 * 1024;
/// At most this many photos are compared: the largest ones.
const MAX_PHOTOS: usize = 20_000;
/// Fingerprints differing in at most this many of their 128 bits match.
const THRESHOLD: u32 = 12;
/// How much of a file is read to find its EXIF preview.
const HEAD: u64 = 160 * 1024;
/// Longest side of the previews shown in the list.
const PREVIEW: u32 = 112;
/// Previews are made for this many photos at most.
const MAX_PREVIEWS: usize = 400;

/// A photo to compare.
pub struct Photo {
    pub node: NodeId,
    pub path: PathBuf,
    pub size: u64,
    pub modified: i64,
}

/// Photos that look alike, largest first.
pub struct Group {
    pub photos: Vec<NodeId>,
    pub sizes: Vec<u64>,
    /// Small versions to show, where there was room for one.
    pub previews: Vec<Option<Pixels>>,
}

/// How far a search has got; shared with the thread doing it.
#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub done: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    pub fn new() -> Progress {
        Progress::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }

    fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

/// The photos under `under` worth comparing, oldest first. Hidden folders
/// (thumbnail caches, trashed files) and apps' private data are skipped.
pub fn candidates(tree: &Tree, under: NodeId) -> Vec<Photo> {
    let mut photos: Vec<Photo> = tree
        .descendants(under)
        .filter(|&id| {
            let node = tree.node(id);
            !node.is_dir()
                && (MIN_PHOTO..=MAX_FILE).contains(&node.size)
                && is_photo(&tree.name(id))
                && !skipped(tree, id, under)
        })
        .map(|id| Photo {
            node: id,
            path: tree.path(id),
            size: tree.node(id).size,
            modified: tree.node(id).modified,
        })
        .collect();
    if photos.len() > MAX_PHOTOS {
        photos.sort_by_key(|p| std::cmp::Reverse(p.size));
        photos.truncate(MAX_PHOTOS);
    }
    photos.sort_by_key(|p| p.modified);
    photos
}

fn is_photo(name: &str) -> bool {
    name.rsplit_once('.').is_some_and(|(_, ext)| {
        ["jpg", "jpeg", "png", "webp"]
            .iter()
            .any(|known| ext.eq_ignore_ascii_case(known))
    })
}

/// Whether `id` is hidden or in `Android/data` or `Android/obb` below `under`.
fn skipped(tree: &Tree, id: NodeId, under: NodeId) -> bool {
    let mut below: Option<String> = None;
    for ancestor in tree.ancestors(id) {
        if ancestor == under {
            break;
        }
        let name = tree.name(ancestor);
        if name.starts_with('.')
            || (name == "Android" && matches!(below.as_deref(), Some("data" | "obb")))
        {
            return true;
        }
        below = Some(name.into_owned());
    }
    false
}

/// Fingerprints `photos` on several threads and groups the ones that look
/// alike, the most space first. Returns nothing if cancelled.
pub fn find(photos: &[Photo], progress: &Progress) -> Vec<Group> {
    progress.files.store(photos.len() as u64, Ordering::Relaxed);
    let next = AtomicUsize::new(0);
    let prints: Mutex<Vec<Option<Print>>> = Mutex::new(vec![None; photos.len()]);
    // Few threads: a photo without an EXIF preview is decoded whole, and
    // a 50 MP one takes 150 MB while it is.
    let threads = std::thread::available_parallelism().map_or(2, |n| n.get().min(4));
    std::thread::scope(|s| {
        for _ in 0..threads {
            s.spawn(|| {
                while !progress.cancelled() {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(photo) = photos.get(i) else {
                        break;
                    };
                    let print = fingerprint(&photo.path);
                    prints.lock().unwrap_or_else(|e| e.into_inner())[i] = print;
                    progress.done.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    if progress.cancelled() {
        return Vec::new();
    }
    let prints = prints.into_inner().unwrap_or_else(|e| e.into_inner());

    let mut groups: Vec<Vec<usize>> = group(&prints);
    for members in &mut groups {
        members.sort_by_key(|&i| std::cmp::Reverse(photos[i].size));
    }
    let extra =
        |members: &Vec<usize>| -> u64 { members[1..].iter().map(|&i| photos[i].size).sum() };
    groups.sort_by_key(|members| std::cmp::Reverse(extra(members)));

    let mut previews_left = MAX_PREVIEWS;
    groups
        .into_iter()
        .map(|members| {
            let previews = members
                .iter()
                .map(|&i| {
                    if previews_left == 0 || progress.cancelled() {
                        return None;
                    }
                    previews_left -= 1;
                    preview(&photos[i].path)
                })
                .collect();
            Group {
                photos: members.iter().map(|&i| photos[i].node).collect(),
                sizes: members.iter().map(|&i| photos[i].size).collect(),
                previews,
            }
        })
        .collect()
}

/// Indices of the photos that look alike, in groups of two or more. Each
/// photo joins the group whose first photo is closest to it, so a slow
/// drift through a series doesn't chain unrelated photos together.
fn group(prints: &[Option<Print>]) -> Vec<Vec<usize>> {
    let mut leaders: Vec<Print> = Vec::new();
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (i, print) in prints.iter().enumerate() {
        let Some(print) = print else {
            continue;
        };
        let closest = leaders
            .iter()
            .enumerate()
            .map(|(g, leader)| (g, leader.distance(print)))
            .filter(|&(_, d)| d <= THRESHOLD)
            .min_by_key(|&(_, d)| d);
        match closest {
            Some((g, _)) => groups[g].push(i),
            None => {
                leaders.push(*print);
                groups.push(vec![i]);
            }
        }
    }
    groups.retain(|members| members.len() > 1);
    groups
}

/// What a picture looks like, roughly: whether brightness rises or falls
/// between neighbouring cells of a 9×9 grid, across and down.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Print {
    across: u64,
    down: u64,
    /// Taller than wide, as shown.
    tall: bool,
}

impl Print {
    fn distance(&self, other: &Print) -> u32 {
        if self.tall != other.tall {
            return u32::MAX;
        }
        (self.across ^ other.across).count_ones() + (self.down ^ other.down).count_ones()
    }

    /// From a 9×9 grid of brightness averages, row by row. Pictures with
    /// next to no contrast (black frames, blank pages) get none: everything
    /// would match them.
    fn of(grid: &[u32; 81], tall: bool) -> Option<Print> {
        let min = grid.iter().min()?;
        let max = grid.iter().max()?;
        if max - min < 8 {
            return None;
        }
        let (mut across, mut down) = (0u64, 0u64);
        for row in 0..8 {
            for col in 0..8 {
                let bit = 1u64 << (row * 8 + col);
                let here = grid[row * 9 + col];
                if here < grid[row * 9 + col + 1] {
                    across |= bit;
                }
                if here < grid[(row + 1) * 9 + col] {
                    down |= bit;
                }
            }
        }
        Some(Print { across, down, tall })
    }
}

fn fingerprint(path: &Path) -> Option<Print> {
    let (image, orientation) = load(path)?;
    let luma = image.to_luma8();
    let grid = grid(luma.as_raw(), luma.width() as usize, luma.height() as usize)?;
    let tall = (luma.height() > luma.width()) != (5..=8).contains(&orientation);
    Print::of(&turn(&grid, orientation), tall)
}

/// The picture to fingerprint, and its EXIF orientation (1 if none): the
/// EXIF preview of a JPEG if it has one, else the whole picture.
fn load(path: &Path) -> Option<(DynamicImage, u16)> {
    let mut head = Vec::new();
    File::open(path)
        .ok()?
        .take(HEAD)
        .read_to_end(&mut head)
        .ok()?;
    let exif = exif(&head);
    let thumbnail = exif.thumbnail.and_then(|range| {
        image::load_from_memory_with_format(&head[range], ImageFormat::Jpeg).ok()
    });
    let image = match thumbnail {
        Some(image) => image,
        None => {
            let mut reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
            let mut limits = Limits::default();
            limits.max_image_width = Some(20_000);
            limits.max_image_height = Some(20_000);
            limits.max_alloc = Some(256 * 1024 * 1024);
            reader.limits(limits);
            reader.decode().ok()?
        }
    };
    Some((image, exif.orientation))
}

/// A small version of the photo, the right way up.
fn preview(path: &Path) -> Option<Pixels> {
    let (image, orientation) = load(path)?;
    let image = image.thumbnail(PREVIEW, PREVIEW);
    let image = match orientation {
        2 => image.fliph(),
        3 => image.rotate180(),
        4 => image.flipv(),
        5 => image.rotate90().fliph(),
        6 => image.rotate90(),
        7 => image.rotate270().fliph(),
        8 => image.rotate270(),
        _ => image,
    }
    .to_rgba8();
    Some(Pixels {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

/// Average brightness of each cell of a 9×9 grid over the picture.
fn grid(luma: &[u8], width: usize, height: usize) -> Option<[u32; 81]> {
    if width < 9 || height < 9 || luma.len() < width * height {
        return None;
    }
    let mut sums = [0u64; 81];
    let mut counts = [0u64; 81];
    let cols: Vec<usize> = (0..width).map(|x| x * 9 / width).collect();
    for (y, line) in luma.chunks_exact(width).take(height).enumerate() {
        let row = y * 9 / height * 9;
        for (&col, &value) in cols.iter().zip(line) {
            sums[row + col] += u64::from(value);
            counts[row + col] += 1;
        }
    }
    Some(std::array::from_fn(|i| (sums[i] / counts[i].max(1)) as u32))
}

/// The grid as the picture is shown, given its EXIF orientation.
fn turn(grid: &[u32; 81], orientation: u16) -> [u32; 81] {
    std::array::from_fn(|i| {
        let (r, c) = (i / 9, i % 9);
        let (sr, sc) = match orientation {
            2 => (r, 8 - c),
            3 => (8 - r, 8 - c),
            4 => (8 - r, c),
            5 => (c, r),
            6 => (8 - c, r),
            7 => (8 - c, 8 - r),
            8 => (c, 8 - r),
            _ => (r, c),
        };
        grid[sr * 9 + sc]
    })
}

/// What a JPEG's EXIF data says.
struct Exif {
    orientation: u16,
    /// Where the embedded preview JPEG is in the bytes.
    thumbnail: Option<std::ops::Range<usize>>,
}

/// Reads the EXIF block at the start of a JPEG file.
fn exif(bytes: &[u8]) -> Exif {
    let mut found = Exif {
        orientation: 1,
        thumbnail: None,
    };
    if !bytes.starts_with(&[0xFF, 0xD8]) {
        return found;
    }
    let mut pos = 2;
    while pos + 4 <= bytes.len() && bytes[pos] == 0xFF {
        let marker = bytes[pos + 1];
        // Start of the picture data: no more headers.
        if marker == 0xDA || marker == 0xD9 {
            break;
        }
        let len = usize::from(u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]));
        let start = pos + 4;
        let end = (pos + 2 + len).min(bytes.len());
        if marker == 0xE1 && end > start + 6 && bytes[start..].starts_with(b"Exif\0\0") {
            let tiff = start + 6;
            read_tiff(bytes, tiff, end, &mut found);
            break;
        }
        pos += 2 + len;
    }
    found
}

/// Reads the orientation (IFD0) and preview location (IFD1) of the TIFF
/// structure in `bytes[tiff..end]`.
fn read_tiff(bytes: &[u8], tiff: usize, end: usize, found: &mut Exif) {
    let data = &bytes[tiff..end];
    let little = match data.get(..2) {
        Some(b"II") => true,
        Some(b"MM") => false,
        _ => return,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = data.get(at..at + 2)?;
        Some(if little {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = data.get(at..at + 4)?;
        Some(if little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };
    if u16_at(2) != Some(42) {
        return;
    }
    // Entries of an IFD: (tag, first value as u16, value or offset as u32).
    let entries = |ifd: usize| -> Vec<(u16, u16, u32)> {
        let count = u16_at(ifd).unwrap_or(0) as usize;
        (0..count)
            .filter_map(|i| {
                let at = ifd + 2 + i * 12;
                Some((u16_at(at)?, u16_at(at + 8)?, u32_at(at + 8)?))
            })
            .collect()
    };
    let Some(ifd0) = u32_at(4).map(|o| o as usize) else {
        return;
    };
    for (tag, short, _) in entries(ifd0) {
        if tag == 0x0112 && (1..=8).contains(&short) {
            found.orientation = short;
        }
    }
    let count0 = u16_at(ifd0).unwrap_or(0) as usize;
    let Some(ifd1) = u32_at(ifd0 + 2 + count0 * 12).map(|o| o as usize) else {
        return;
    };
    if ifd1 == 0 {
        return;
    }
    let (mut offset, mut length) = (None, None);
    for (tag, _, long) in entries(ifd1) {
        match tag {
            0x0201 => offset = Some(long as usize),
            0x0202 => length = Some(long as usize),
            _ => {}
        }
    }
    if let (Some(offset), Some(length)) = (offset, length)
        && offset + length <= data.len()
        && data[offset..].starts_with(&[0xFF, 0xD8])
    {
        found.thumbnail = Some(tiff + offset..tiff + offset + length);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use disktree_core::{ScanOptions, ScanProgress, scan};
    use image::{GrayImage, Luma, RgbImage};

    use super::*;

    /// A test picture: a few soft blobs whose places depend on `seed`.
    fn picture(seed: u32, width: u32, height: u32) -> RgbImage {
        let blobs: Vec<(f32, f32, f32)> = (0..4)
            .map(|i| {
                let k = (seed * 7 + i * 13) as f32;
                (
                    (k * 37.0 % 100.0) / 100.0,
                    (k * 53.0 % 100.0) / 100.0,
                    0.1 + (k * 11.0 % 20.0) / 100.0,
                )
            })
            .collect();
        RgbImage::from_fn(width, height, |x, y| {
            let (u, v) = (x as f32 / width as f32, y as f32 / height as f32);
            let light: f32 = blobs
                .iter()
                .map(|&(bx, by, r)| (-((u - bx).powi(2) + (v - by).powi(2)) / (r * r)).exp())
                .sum();
            let value = (40.0 + light.min(1.0) * 200.0) as u8;
            image::Rgb([value, value / 2, 255 - value])
        })
    }

    fn jpeg(image: &RgbImage, quality: u8) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality)
            .encode_image(image)
            .unwrap();
        bytes
    }

    /// A JPEG with an EXIF block holding an orientation and a preview.
    fn with_exif(main: &[u8], thumb: &[u8], orientation: u16) -> Vec<u8> {
        // Little-endian TIFF: header, IFD0 with one entry, IFD1 with two.
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"II");
        tiff.extend_from_slice(&42u16.to_le_bytes());
        tiff.extend_from_slice(&8u32.to_le_bytes());
        let ifd1 = 8 + 2 + 12 + 4;
        let thumb_at = ifd1 + 2 + 2 * 12 + 4;
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x0112u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&u32::from(orientation).to_le_bytes());
        tiff.extend_from_slice(&(ifd1 as u32).to_le_bytes());
        tiff.extend_from_slice(&2u16.to_le_bytes());
        for (tag, value) in [(0x0201u16, thumb_at as u32), (0x0202, thumb.len() as u32)] {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&4u16.to_le_bytes());
            tiff.extend_from_slice(&1u32.to_le_bytes());
            tiff.extend_from_slice(&value.to_le_bytes());
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(thumb);

        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&main[2..]);
        out
    }

    #[test]
    fn reads_exif_previews_and_orientation() {
        let main = jpeg(&picture(1, 400, 300), 90);
        let thumb = jpeg(&picture(1, 160, 120), 80);
        let file = with_exif(&main, &thumb, 6);
        let found = exif(&file);
        assert_eq!(found.orientation, 6);
        assert_eq!(&file[found.thumbnail.unwrap()], thumb.as_slice());
        let plain = exif(&main);
        assert_eq!(plain.orientation, 1);
        assert!(plain.thumbnail.is_none());
        assert!(exif(b"not a jpeg").thumbnail.is_none());
    }

    #[test]
    fn grids_turn_with_the_orientation() {
        let grid: [u32; 81] = std::array::from_fn(|i| i as u32);
        // Turned a quarter clockwise, the bottom-left cell comes first.
        assert_eq!(turn(&grid, 6)[0], 72);
        assert_eq!(turn(&grid, 8)[0], 8);
        assert_eq!(turn(&grid, 1), grid);
        assert_eq!(turn(&turn(&grid, 6), 8), grid);
    }

    #[test]
    fn flat_pictures_have_no_fingerprint() {
        let flat = GrayImage::from_pixel(50, 50, Luma([128]));
        let grid = grid(flat.as_raw(), 50, 50).unwrap();
        assert!(Print::of(&grid, false).is_none());
    }

    #[test]
    fn groups_photos_that_look_alike() {
        let dir = std::env::temp_dir().join(format!("disktree-similar-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Camera")).unwrap();
        fs::create_dir_all(dir.join(".thumbnails")).unwrap();
        let first = picture(3, 800, 600);
        // The same scene again: smaller, saved at a lower quality.
        let again = image::imageops::resize(&first, 640, 480, image::imageops::Triangle);
        let other = picture(9, 800, 600);
        let pad = |mut bytes: Vec<u8>| {
            // Every photo must pass the minimum size.
            if bytes.len() < MIN_PHOTO as usize {
                bytes.extend(std::iter::repeat_n(0, MIN_PHOTO as usize - bytes.len()));
            }
            bytes
        };
        fs::write(dir.join("Camera/a.jpg"), pad(jpeg(&first, 95))).unwrap();
        fs::write(dir.join("Camera/b.jpg"), pad(jpeg(&again, 60))).unwrap();
        fs::write(dir.join("Camera/c.jpg"), pad(jpeg(&other, 95))).unwrap();
        // With an EXIF preview and stored sideways, shown upright.
        let sideways = image::imageops::rotate270(&first);
        let thumb = image::imageops::resize(&sideways, 120, 160, image::imageops::Triangle);
        fs::write(
            dir.join("Camera/d.jpg"),
            pad(with_exif(&jpeg(&sideways, 90), &jpeg(&thumb, 80), 6)),
        )
        .unwrap();
        fs::write(dir.join(".thumbnails/a.jpg"), pad(jpeg(&first, 95))).unwrap();
        let tree = scan(&dir, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;

        let photos = candidates(&tree, tree.root());
        assert_eq!(photos.len(), 4, "the hidden folder is skipped");
        let groups = find(&photos, &Progress::new());
        assert_eq!(groups.len(), 1);
        let mut names: Vec<String> = groups[0]
            .photos
            .iter()
            .map(|&id| tree.name(id).into_owned())
            .collect();
        names.sort();
        assert_eq!(names, ["a.jpg", "b.jpg", "d.jpg"]);
        let preview = groups[0].previews[0].as_ref().unwrap();
        assert!(preview.width <= PREVIEW && preview.height <= PREVIEW);
        assert!(groups[0].sizes[0] >= groups[0].sizes[1]);
        let _ = fs::remove_dir_all(&dir);
    }
}
