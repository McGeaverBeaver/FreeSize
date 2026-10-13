//! A fuller scan when the user's rights left folders unread. FreeSize runs
//! without extra rights by default: an elevated program on Windows doesn't
//! see the user's mapped drives, and every delete is riskier. When a scan
//! misses folders, the Clean up tab names them and offers, per platform:
//!
//! - Windows: a new FreeSize window as administrator, through UAC, which
//!   turns on the backup privilege and says so in its title.
//! - macOS: Full Disk Access first (a setting), then an administrator scan:
//!   `freesize-gui --export` runs as root through an administrator prompt
//!   and the result opens as a saved scan.
//! - Linux: the same root scan through `pkexec`; never the window as root.
//! - Android: nothing; apps' private data comes through Usage access.

use std::path::{Path, PathBuf};
#[cfg(all(unix, not(target_os = "android")))]
use std::process::Child;

use freesize_core::access::{self, Rights};

/// Button texts of the unreadable-folders card, which the controller tells
/// apart.
pub const SCAN_AS_ADMIN: &str = "Scan as administrator";
pub const SCAN_AS_ROOT: &str = "Scan as root";
pub const FULL_DISK_ACCESS: &str = "Open Full Disk Access";

/// What the Clean up tab offers under the folders a scan couldn't read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Offer {
    /// The button's text; empty when nothing can be offered.
    pub action: String,
    /// Why folders were missed and what the button does.
    pub note: String,
}

impl Offer {
    fn new(action: &str, note: String) -> Offer {
        Offer {
            action: action.into(),
            note,
        }
    }

    fn none(note: String) -> Offer {
        Offer::new("", note)
    }
}

/// The offer for a scan of `root` made with `rights`. A saved scan gets
/// none: it was made elsewhere, perhaps with other rights.
pub fn offer(rights: Rights, root: &Path, imported: bool) -> Offer {
    if imported {
        return Offer::none(
            "This is a saved scan; whoever made it couldn't read these folders.".into(),
        );
    }
    let beyond = access::beyond_reach().map_or(String::new(), |s| format!(" {s}"));
    if cfg!(target_os = "android") {
        return Offer::none(
            "Android keeps apps' private data closed to other apps; the home screen measures \
             it through Usage access instead."
                .into(),
        );
    }
    if cfg!(windows) {
        return match rights {
            Rights::User => Offer::new(
                SCAN_AS_ADMIN,
                format!(
                    "Windows keeps other users' folders and some of its own from FreeSize. Scan \
                     as administrator opens a new FreeSize window, after Windows asks, that reads \
                     them; deleting there follows the same rules.{beyond}"
                ),
            ),
            _ => Offer::none(format!(
                "This window runs as administrator and still couldn't read them.{beyond}"
            )),
        };
    }
    if cfg!(target_os = "macos") {
        if full_disk_access() == Some(false) {
            return Offer::new(
                FULL_DISK_ACCESS,
                "macOS keeps Mail, Messages, Safari and other apps' data from FreeSize until it \
                 has Full Disk Access, under Privacy & Security. Turn it on for FreeSize, then \
                 scan again."
                    .into(),
            );
        }
        return match rights {
            Rights::User => Offer::new(
                SCAN_AS_ADMIN,
                format!(
                    "Other users' folders need an administrator. Scan as administrator asks for \
                     your password, scans {} in the background and opens the result as a saved \
                     scan, which can't open or delete anything.",
                    root.display()
                ),
            ),
            _ => Offer::none("FreeSize runs as root and still couldn't read them.".into()),
        };
    }
    match rights {
        Rights::User if pkexec_available() => Offer::new(
            SCAN_AS_ROOT,
            format!(
                "Other users' folders and some of the system's need root. Scan as root asks for \
                 your password, scans {} in the background and opens the result as a saved scan, \
                 which can't open or delete anything.",
                root.display()
            ),
        ),
        Rights::User => Offer::none(format!(
            "Other users' folders and some of the system's need root: run `sudo freesize {} \
             --export scan.json` and open the file here.",
            root.display()
        )),
        _ => Offer::none("FreeSize runs as root and still couldn't read them.".into()),
    }
}

/// Whether macOS lets this app read what Full Disk Access protects; `None`
/// elsewhere, or when it can't tell.
pub fn full_disk_access() -> Option<bool> {
    #[cfg(target_os = "macos")]
    {
        // TCC's own database is readable only with Full Disk Access.
        let home = std::env::home_dir()?;
        let probe = home.join("Library/Application Support/com.apple.TCC/TCC.db");
        match std::fs::File::open(&probe) {
            Ok(_) => Some(true),
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Some(false),
            Err(_) => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

fn pkexec_available() -> bool {
    if !cfg!(target_os = "linux") {
        return false;
    }
    std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("pkexec").is_file()))
}

/// What a button started.
pub enum Started {
    /// Windows: a new window is opening, after UAC; this one carries on.
    NewWindow,
    /// A root scan runs in the background; poll it.
    #[cfg_attr(not(all(unix, not(target_os = "android"))), allow(dead_code))]
    Exporting(Exporting),
    /// A setting opened; scan again once it's changed.
    Opened,
}

/// A root scan writing a scan file in the background.
pub struct Exporting {
    #[cfg(all(unix, not(target_os = "android")))]
    child: Child,
    pub file: PathBuf,
    pub root: PathBuf,
}

impl Exporting {
    /// `Some` once the scan ended: the file to open, or why there is none.
    pub fn poll(&mut self) -> Option<Result<PathBuf, String>> {
        #[cfg(all(unix, not(target_os = "android")))]
        {
            match self.child.try_wait() {
                Ok(None) => None,
                Ok(Some(status)) if status.success() && self.file.is_file() => {
                    Some(Ok(self.file.clone()))
                }
                Ok(Some(status)) => Some(Err(match status.code() {
                    // pkexec: the password dialog was dismissed or refused.
                    Some(126) | Some(127) => "not allowed".to_string(),
                    Some(code) => format!("the scan ended with code {code}"),
                    None => "the scan was stopped".to_string(),
                })),
                Err(err) => Some(Err(err.to_string())),
            }
        }
        #[cfg(not(all(unix, not(target_os = "android"))))]
        {
            Some(Err(format!(
                "{}: not on this platform",
                self.file.display()
            )))
        }
    }
}

/// Does what the card's button says for a scan of `root`.
pub fn start(action: &str, root: &Path) -> Result<Started, String> {
    match action {
        FULL_DISK_ACCESS => open_full_disk_access().map(|()| Started::Opened),
        SCAN_AS_ADMIN if cfg!(windows) => {
            relaunch_as_administrator(root).map(|()| Started::NewWindow)
        }
        SCAN_AS_ADMIN | SCAN_AS_ROOT => root_scan(root).map(Started::Exporting),
        _ => Err("nothing to do".into()),
    }
}

fn open_full_disk_access() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("only on macOS".into())
    }
}

/// Windows: starts this program again, elevated, scanning `root`.
#[cfg_attr(not(windows), allow(unused_variables))]
fn relaunch_as_administrator(root: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use std::ptr::null;

        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let wide = |s: &std::ffi::OsStr| -> Vec<u16> { s.encode_wide().chain([0]).collect() };
        let verb = wide("runas".as_ref());
        let file = wide(exe.as_os_str());
        // The path is one argument whatever spaces it holds.
        let parameters = wide(
            format!(
                "{ELEVATED_FLAG} \"{}\"",
                root.display().to_string().replace('"', "")
            )
            .as_ref(),
        );
        // SAFETY: every string is NUL-terminated and lives through the call.
        let result = unsafe {
            ShellExecuteW(
                null::<core::ffi::c_void>().cast_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                parameters.as_ptr(),
                null(),
                SW_SHOWNORMAL,
            )
        };
        // Values up to 32 are error codes; 5 means Windows (or the user at
        // the UAC prompt) said no.
        match result as usize {
            n if n > 32 => Ok(()),
            5 => Err("Windows didn't allow it".into()),
            n => Err(format!("Windows couldn't start it (error {n})")),
        }
    }
    #[cfg(not(windows))]
    {
        Err("only on Windows".into())
    }
}

/// The flag the elevated window is started with.
pub const ELEVATED_FLAG: &str = "--administrator";
/// The flag of the windowless scan: `freesize-gui --export FILE PATH`.
pub const EXPORT_FLAG: &str = "--export";

/// Scans `root` as root in the background, into a file in the app's data
/// folder; older such files go first, since each can be large.
#[cfg_attr(not(all(unix, not(target_os = "android"))), allow(unused_variables))]
fn root_scan(root: &Path) -> Result<Exporting, String> {
    #[cfg(all(unix, not(target_os = "android")))]
    {
        let dir = crate::growth::data_dir()
            .ok_or("no folder to put the scan in")?
            .join("root-scans");
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        if let Ok(old) = std::fs::read_dir(&dir) {
            for entry in old.flatten() {
                let _ = std::fs::remove_file(entry.path());
            }
        }
        let file = dir.join(format!(
            "{}-{}.json",
            root.file_name()
                .map_or_else(|| "root".to_string(), |n| n.to_string_lossy().into_owned()),
            freesize_agent::now()
        ));
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut command = if cfg!(target_os = "macos") {
            // osascript asks for an administrator's password.
            let shell = [
                exe.as_os_str(),
                EXPORT_FLAG.as_ref(),
                file.as_os_str(),
                root.as_os_str(),
            ]
            .iter()
            .map(|s| shell_quote(&s.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(" ");
            let mut osascript = std::process::Command::new("osascript");
            osascript.arg("-e").arg(format!(
                "do shell script \"{}\" with administrator privileges",
                shell.replace('\\', "\\\\").replace('"', "\\\"")
            ));
            osascript
        } else {
            let mut pkexec = std::process::Command::new("pkexec");
            pkexec.arg(&exe).arg(EXPORT_FLAG).arg(&file).arg(root);
            pkexec
        };
        let child = command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("couldn't start the scan: {e}"))?;
        Ok(Exporting {
            child,
            file,
            root: root.to_path_buf(),
        })
    }
    #[cfg(not(all(unix, not(target_os = "android"))))]
    {
        Err("not on this platform".into())
    }
}

/// `s` as one word for `sh`.
#[cfg(all(unix, not(target_os = "android")))]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_scan_gets_no_button() {
        let offer = offer(Rights::User, Path::new("/"), true);
        assert!(offer.action.is_empty());
        assert!(offer.note.contains("saved scan"));
    }

    #[test]
    fn root_is_offered_nothing_more() {
        let offer = offer(Rights::Root, Path::new("/"), false);
        if !cfg!(target_os = "android") && !cfg!(windows) {
            assert!(offer.action.is_empty());
            assert!(offer.note.contains("root"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_user_is_offered_a_root_scan_where_pkexec_exists() {
        let offer = offer(Rights::User, Path::new("/home"), false);
        if pkexec_available() {
            assert_eq!(offer.action, SCAN_AS_ROOT);
            assert!(offer.note.contains("/home"));
        } else {
            assert!(offer.action.is_empty());
            assert!(offer.note.contains("sudo freesize /home --export"));
        }
    }

    #[cfg(all(unix, not(target_os = "android")))]
    #[test]
    fn shell_words_survive_quotes_and_spaces() {
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }
}
