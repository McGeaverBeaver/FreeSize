// Don't open a console window next to the app on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> Result<(), slint::PlatformError> {
    // `freesize-gui [PATH]` scans PATH immediately.
    let initial = std::env::args_os().nth(1).map(std::path::PathBuf::from);
    freesize_app::run(initial)
}
