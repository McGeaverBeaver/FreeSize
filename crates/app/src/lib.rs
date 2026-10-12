//! DiskTree app: the Slint UI for desktop (Windows, macOS, Linux) and Android.

slint::include_modules!();

mod actions;
#[cfg(target_os = "android")]
mod android;
mod appbubbles;
mod apps;
mod changes;
mod cleanup;
mod controller;
mod filecleanup;
mod growth;
mod hidden;
mod history;
mod home;
mod insights;
mod locations;
mod moves;
mod owners;
mod report;
mod rows;
mod scans;
mod similar;
#[cfg(windows)]
mod winicon;

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
    android::install_panic_hook();
    android::init(app.clone());
    // Fails if Android restarted the activity inside a process that already
    // set up Slint; the manifest's configChanges and singleTask launch mode
    // make that rare, and returning lets the activity close cleanly.
    if let Err(err) = slint::android::init(app) {
        android::log(&format!("could not start the Slint Android backend: {err}"));
        return;
    }
    if let Err(err) = run(None) {
        android::log(&format!("app exited with an error: {err}"));
    }
}
