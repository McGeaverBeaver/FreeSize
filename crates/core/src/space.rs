//! Free and total space of the volume holding a path.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskSpace {
    pub total: u64,
    /// Space available to the current user.
    pub free: u64,
}

impl DiskSpace {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
}

/// Size of the volume containing `path`, or `None` if it can't be queried.
#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
pub fn disk_space(path: &Path) -> Option<DiskSpace> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs only writes into the zeroed struct we pass.
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = if stats.f_frsize > 0 {
        stats.f_frsize as u64
    } else {
        stats.f_bsize as u64
    };
    Some(DiskSpace {
        total: stats.f_blocks as u64 * block,
        free: stats.f_bavail as u64 * block,
    })
}

/// Size of the volume containing `path`, or `None` if it can't be queried.
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<DiskSpace> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let (mut available, mut total, mut free) = (0u64, 0u64, 0u64);
    // SAFETY: `wide` is NUL-terminated and the out-pointers are valid.
    let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut available, &mut total, &mut free) };
    (ok != 0).then_some(DiskSpace {
        total,
        free: available,
    })
}

#[cfg(not(any(unix, windows)))]
pub fn disk_space(_path: &Path) -> Option<DiskSpace> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn current_dir_has_space() {
        let space = super::disk_space(&std::env::current_dir().unwrap()).unwrap();
        assert!(space.total > 0);
        assert!(space.free <= space.total);
    }
}
