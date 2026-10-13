// Don't open a console window next to the app on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

fn main() -> Result<(), slint::PlatformError> {
    // `freesize-gui [PATH]` scans PATH immediately; `--administrator PATH`
    // is the window opened through UAC, and `--export FILE PATH` scans
    // without a window (the root scan on macOS and Linux).
    match freesize_app::launch(std::env::args_os().skip(1)) {
        freesize_app::Launch::Window { path, elevated } => freesize_app::run(path, elevated),
        freesize_app::Launch::Export { file, path } => {
            if let Err(err) = freesize_app::export_scan(&path, &file) {
                eprintln!("freesize-gui: {err}");
                std::process::exit(1);
            }
            Ok(())
        }
    }
}
