//! The OS-specific parts of reading an entry's metadata.

use std::fs::Metadata;
use std::path::Path;

/// What the scanner needs from an entry's metadata beyond its type and length.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct EntryInfo {
    /// Bytes allocated on disk.
    pub allocated: u64,
    /// Seconds since the Unix epoch.
    pub modified: i64,
    /// Filesystem the entry lives on, used to stay on one filesystem.
    pub device: u64,
    /// `(device, inode)` for files with more than one hard link.
    pub hardlink: Option<(u64, u64)>,
    /// Cloud placeholder whose data is not stored locally.
    pub cloud: bool,
}

/// Per-scan facts about the volume being scanned.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Volume {
    #[cfg_attr(not(windows), allow(dead_code))]
    pub cluster_size: u64,
}

#[cfg(unix)]
pub(crate) fn volume(_root: &Path) -> Volume {
    Volume { cluster_size: 4096 }
}

#[cfg(unix)]
pub(crate) fn entry_info(
    meta: &Metadata,
    _path: &dyn Fn() -> std::path::PathBuf,
    _volume: &Volume,
) -> EntryInfo {
    use std::os::unix::fs::MetadataExt;

    #[cfg_attr(not(target_os = "macos"), allow(unused_mut))]
    let mut info = EntryInfo {
        allocated: meta.blocks() * 512,
        modified: meta.mtime(),
        device: meta.dev(),
        hardlink: (!meta.is_dir() && meta.nlink() > 1).then(|| (meta.dev(), meta.ino())),
        cloud: false,
    };

    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt as _;
        // iCloud "Optimize Mac Storage" leaves dataless files behind.
        const SF_DATALESS: u32 = 0x4000_0000;
        info.cloud = meta.st_flags() & SF_DATALESS != 0;
    }

    info
}

#[cfg(windows)]
pub(crate) fn volume(root: &Path) -> Volume {
    Volume {
        cluster_size: windows::cluster_size(root).unwrap_or(4096),
    }
}

#[cfg(windows)]
pub(crate) fn entry_info(
    meta: &Metadata,
    path: &dyn Fn() -> std::path::PathBuf,
    volume: &Volume,
) -> EntryInfo {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x200;
    const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x800;
    const FILE_ATTRIBUTE_OFFLINE: u32 = 0x1000;
    const FILE_ATTRIBUTE_RECALL_ON_OPEN: u32 = 0x4_0000;
    const FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS: u32 = 0x40_0000;

    let attrs = meta.file_attributes();
    let len = meta.file_size();
    let cloud = attrs
        & (FILE_ATTRIBUTE_OFFLINE
            | FILE_ATTRIBUTE_RECALL_ON_OPEN
            | FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS)
        != 0;
    let allocated = if meta.is_dir() || cloud {
        0
    } else if attrs & (FILE_ATTRIBUTE_SPARSE_FILE | FILE_ATTRIBUTE_COMPRESSED) != 0 {
        windows::compressed_size(&path()).unwrap_or(len)
    } else {
        len.div_ceil(volume.cluster_size) * volume.cluster_size
    };

    // FILETIME counts 100 ns intervals since 1601-01-01.
    const EPOCH_DIFF_SECS: i64 = 11_644_473_600;
    let modified = (meta.last_write_time() / 10_000_000) as i64 - EPOCH_DIFF_SECS;

    EntryInfo {
        allocated,
        modified,
        // Volume mount points are reparse points, which are never followed,
        // so every entry reached is on the root's volume.
        device: 0,
        hardlink: None,
        cloud,
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{GetLastError, NO_ERROR};
    use windows_sys::Win32::Storage::FileSystem::{
        GetCompressedFileSizeW, GetDiskFreeSpaceW, GetVolumePathNameW, INVALID_FILE_SIZE,
    };

    fn wide(path: &Path) -> Vec<u16> {
        let os = path.as_os_str();
        // Raw Win32 calls need the verbatim prefix for paths past MAX_PATH.
        let needs_prefix =
            path.is_absolute() && os.len() >= 248 && !os.as_encoded_bytes().starts_with(b"\\\\");
        let prefix: &[u16] = if needs_prefix { &[92, 92, 63, 92] } else { &[] };
        prefix
            .iter()
            .copied()
            .chain(os.encode_wide())
            .chain(Some(0))
            .collect()
    }

    pub fn compressed_size(path: &Path) -> Option<u64> {
        let wide = wide(path);
        let mut high = 0u32;
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &mut high) };
        if low == INVALID_FILE_SIZE && unsafe { GetLastError() } != NO_ERROR {
            return None;
        }
        Some((u64::from(high) << 32) | u64::from(low))
    }

    pub fn cluster_size(root: &Path) -> Option<u64> {
        let wide = wide(root);
        let mut volume = [0u16; 512];
        // SAFETY: both buffers are valid for the lengths passed.
        let ok =
            unsafe { GetVolumePathNameW(wide.as_ptr(), volume.as_mut_ptr(), volume.len() as u32) };
        if ok == 0 {
            return None;
        }
        let (mut sectors, mut bytes, mut free, mut total) = (0u32, 0u32, 0u32, 0u32);
        // SAFETY: `volume` is NUL-terminated by GetVolumePathNameW.
        let ok = unsafe {
            GetDiskFreeSpaceW(
                volume.as_ptr(),
                &mut sectors,
                &mut bytes,
                &mut free,
                &mut total,
            )
        };
        let size = u64::from(sectors) * u64::from(bytes);
        (ok != 0 && size > 0).then_some(size)
    }
}
