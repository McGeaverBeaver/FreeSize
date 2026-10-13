//! FreeSize app: the Slint UI for desktop (Windows, macOS, Linux) and Android.

slint::include_modules!();

mod actions;
#[cfg(target_os = "android")]
mod android;
mod appbubbles;
mod apps;
#[cfg(target_os = "android")]
mod background;
mod changes;
mod cleanup;
mod controller;
mod elevate;
mod filecleanup;
mod growth;
mod hidden;
mod history;
mod home;
mod hub;
mod insights;
mod junk;
mod locations;
mod moves;
mod owners;
mod quarantine;
mod report;
mod rows;
mod scans;
mod similar;
mod updater;
#[cfg(windows)]
mod winicon;

use std::path::{Path, PathBuf};

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// The window; scanning `path` right away if given. `elevated` is the
    /// window Windows opened as administrator for one scan.
    Window {
        path: Option<PathBuf>,
        elevated: bool,
    },
    /// No window: scan `path` and write the scan file, for the root scan
    /// macOS and Linux run through an administrator prompt.
    Export { file: PathBuf, path: PathBuf },
}

/// Reads the arguments after the program's name: `[PATH]`,
/// `--administrator PATH` or `--export FILE PATH`.
pub fn launch(args: impl IntoIterator<Item = std::ffi::OsString>) -> Launch {
    let args: Vec<std::ffi::OsString> = args.into_iter().collect();
    let flag = |i: usize| args.get(i).and_then(|a| a.to_str());
    if flag(0) == Some(elevate::EXPORT_FLAG)
        && let (Some(file), Some(path)) = (args.get(1), args.get(2))
    {
        return Launch::Export {
            file: PathBuf::from(file),
            path: PathBuf::from(path),
        };
    }
    let elevated = flag(0) == Some(elevate::ELEVATED_FLAG);
    let path = args
        .get(usize::from(elevated))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty());
    Launch::Window { path, elevated }
}

/// Runs the desktop app. Scans `initial` right away if given. The elevated
/// window turns on Windows' backup privilege, so an administrator's scan
/// lists folders whose permissions leave administrators out.
pub fn run(initial: Option<PathBuf>, elevated: bool) -> Result<(), slint::PlatformError> {
    if elevated {
        freesize_core::access::enable_backup_privilege();
    }
    // The file an update left behind, now that the new program runs.
    updater::tidy();
    let ui = AppWindow::new()?;
    let _controller = controller::install(&ui, initial, elevated);
    ui.run()
}

/// Scans `path` and writes the scan file, without a window. Run as root
/// through `pkexec` or an administrator prompt by [`elevate`].
pub fn export_scan(path: &Path, file: &Path) -> Result<(), String> {
    use freesize_core::{ScanOptions, ScanProgress, scan};
    freesize_core::access::enable_backup_privilege();
    let result = scan(path, &ScanOptions::default(), &ScanProgress::new())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let out = std::fs::File::create(file).map_err(|e| format!("{}: {e}", file.display()))?;
    freesize_core::ncdu::export(
        &result.tree,
        freesize_agent::now(),
        std::io::BufWriter::new(out),
    )
    .map_err(|e| format!("{}: {e}", file.display()))
}

/// Entry point called by Android's NativeActivity.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    android::install_panic_hook();
    android::init(app.clone());
    // Fails if Android restarted the activity inside a process that already
    // set up Slint; the manifest's configChanges and singleTask launch mode
    // make that rare, and returning lets the activity close cleanly.
    if let Err(err) = slint::android::init(app) {
        android::log(&format!("could not start the Slint Android backend: {err}"));
        return;
    }
    if let Err(err) = run(None, false) {
        android::log(&format!("app exited with an error: {err}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launch_modes_parse() {
        let args = |list: &[&str]| launch(list.iter().map(std::ffi::OsString::from));
        assert_eq!(
            args(&[]),
            Launch::Window {
                path: None,
                elevated: false
            }
        );
        assert_eq!(
            args(&["/data"]),
            Launch::Window {
                path: Some(PathBuf::from("/data")),
                elevated: false
            }
        );
        assert_eq!(
            args(&["--administrator", "C:\\"]),
            Launch::Window {
                path: Some(PathBuf::from("C:\\")),
                elevated: true
            }
        );
        assert_eq!(
            args(&["--export", "/tmp/scan.json", "/"]),
            Launch::Export {
                file: PathBuf::from("/tmp/scan.json"),
                path: PathBuf::from("/")
            }
        );
        // An export without its two arguments is no export.
        assert_eq!(
            args(&["--export"]),
            Launch::Window {
                path: Some(PathBuf::from("--export")),
                elevated: false
            }
        );
    }

    #[test]
    fn an_export_is_a_scan_file_with_unreadable_folders_kept() {
        let dir = std::env::temp_dir().join(format!("freesize-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/a.txt"), b"hello").unwrap();
        let file = dir.join("scan.json");
        export_scan(&dir, &file).unwrap();
        let tree = freesize_core::ncdu::import(std::fs::File::open(&file).unwrap()).unwrap();
        assert_eq!(tree.node(tree.root()).files, 1);
        assert!(tree.unreadable().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
