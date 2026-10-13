//! What the running program may read: everything, as root or an elevated
//! administrator, or only what its user may. A scan without the rights
//! marks the folders it couldn't list (see [`crate::Tree::unreadable`]);
//! this module says which rights it had, and on Windows turns on the backup
//! privilege, which lets an administrator list folders whose permissions
//! leave administrators out.

/// Whose eyes a scan sees with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rights {
    /// The signed-in user's: other users' folders and the system's own stay
    /// closed.
    User,
    /// Windows: elevated through UAC.
    Administrator,
    /// Unix: root.
    Root,
}

impl Rights {
    /// "user", "administrator" or "root", as reports and the Hub say it.
    pub fn label(self) -> &'static str {
        match self {
            Rights::User => "user",
            Rights::Administrator => "administrator",
            Rights::Root => "root",
        }
    }

    pub fn parse(label: &str) -> Option<Rights> {
        match label {
            "user" => Some(Rights::User),
            "administrator" => Some(Rights::Administrator),
            "root" => Some(Rights::Root),
            _ => None,
        }
    }

    /// Whether a scan with these rights reads every folder it can reach
    /// (Windows keeps a few from administrators too; see
    /// [`beyond_reach`]).
    pub fn sees_everything(self) -> bool {
        self != Rights::User
    }
}

/// The rights this process runs with.
pub fn rights() -> Rights {
    #[cfg(unix)]
    {
        // SAFETY: geteuid takes nothing and can't fail.
        if unsafe { libc::geteuid() } == 0 {
            Rights::Root
        } else {
            Rights::User
        }
    }
    #[cfg(windows)]
    {
        if windows::elevated() {
            Rights::Administrator
        } else {
            Rights::User
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Rights::User
    }
}

/// Windows: turns on the backup privilege for this process, so listing a
/// folder succeeds whatever its permissions say. Only administrators hold
/// it; returns whether it's on now. Elsewhere nothing to turn on: root
/// reads everything already.
pub fn enable_backup_privilege() -> bool {
    #[cfg(windows)]
    {
        windows::enable_backup_privilege()
    }
    #[cfg(not(windows))]
    {
        rights() == Rights::Root
    }
}

/// What a scan of a whole drive misses even with every right, on this
/// platform, as a sentence; `None` where root sees it all.
pub fn beyond_reach() -> Option<&'static str> {
    if cfg!(windows) {
        Some(
            "System Volume Information (restore points and shadow copies) is closed even to \
             administrators, and Windows' reserved storage, about 7 GB, is seen by no program.",
        )
    } else {
        None
    }
}

#[cfg(windows)]
mod windows {
    use std::ffi::c_void;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LUID, NO_ERROR};
    use windows_sys::Win32::Security::{
        AdjustTokenPrivileges, GetTokenInformation, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
        SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_ELEVATION, TOKEN_PRIVILEGES,
        TOKEN_QUERY, TokenElevation,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// This process's token, closed when dropped.
    struct Token(HANDLE);

    impl Token {
        fn open(access: u32) -> Option<Token> {
            let mut handle: HANDLE = null_mut();
            // SAFETY: the pseudo handle of the current process is always
            // valid; `handle` is written only on success.
            let ok = unsafe { OpenProcessToken(GetCurrentProcess(), access, &mut handle) };
            (ok != 0 && !handle.is_null()).then_some(Token(handle))
        }
    }

    impl Drop for Token {
        fn drop(&mut self) {
            // SAFETY: the handle came from OpenProcessToken and is closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    pub fn elevated() -> bool {
        let Some(token) = Token::open(TOKEN_QUERY) else {
            return false;
        };
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut written = 0u32;
        // SAFETY: the buffer is exactly one TOKEN_ELEVATION, as its length says.
        let ok = unsafe {
            GetTokenInformation(
                token.0,
                TokenElevation,
                (&mut elevation as *mut TOKEN_ELEVATION).cast::<c_void>(),
                std::mem::size_of::<TOKEN_ELEVATION>() as u32,
                &mut written,
            )
        };
        ok != 0 && elevation.TokenIsElevated != 0
    }

    pub fn enable_backup_privilege() -> bool {
        let Some(token) = Token::open(TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY) else {
            return false;
        };
        let name: Vec<u16> = "SeBackupPrivilege\0".encode_utf16().collect();
        let mut luid = LUID {
            LowPart: 0,
            HighPart: 0,
        };
        // SAFETY: `name` is NUL-terminated and outlives the call.
        if unsafe { LookupPrivilegeValueW(null(), name.as_ptr(), &mut luid) } == 0 {
            return false;
        }
        let privileges = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        // SAFETY: `privileges` holds the one entry its count says.
        let ok =
            unsafe { AdjustTokenPrivileges(token.0, 0, &privileges, 0, null_mut(), null_mut()) };
        // The call succeeds even for a privilege the token doesn't hold; it
        // then sets ERROR_NOT_ALL_ASSIGNED.
        // SAFETY: reads this thread's last error.
        ok != 0 && unsafe { GetLastError() } == NO_ERROR
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_round_trip() {
        for rights in [Rights::User, Rights::Administrator, Rights::Root] {
            assert_eq!(Rights::parse(rights.label()), Some(rights));
        }
        assert_eq!(Rights::parse("superuser"), None);
        assert!(!Rights::User.sees_everything());
        assert!(Rights::Root.sees_everything());
    }

    #[test]
    fn this_process_has_some_rights() {
        // Whatever they are, asking must not fail; and the backup privilege
        // can only be on where the process could read everything anyway.
        let rights = rights();
        if enable_backup_privilege() {
            assert!(rights.sees_everything());
        }
    }
}
