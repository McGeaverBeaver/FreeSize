//! DiskTree app: the Slint UI for desktop (Windows, macOS, Linux) and Android.

slint::include_modules!();

mod actions;
#[cfg(target_os = "android")]
mod android;
mod controller;
mod locations;
mod rows;

use std::path::PathBuf;

/// Runs the desktop app. Scans `initial` right away if given.
pub fn run(initial: Option<PathBuf>) -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    let _controller = controller::install(&ui, initial);
    ui.run()
}

/// Entry point called by Android's NativeActivity.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: slint::android::AndroidApp) {
    android::init(app.clone());
    slint::android::init(app).expect("failed to initialize the Slint Android backend");
    if let Err(err) = run(None) {
        eprintln!("DiskTree: {err}");
    }
}
