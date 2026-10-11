//! Rows of the home screen: drives and folders to scan and, where the
//! platform reports it, how the device's storage divides up between apps,
//! the user's files and the system.

use disktree_core::{DiskSpace, disk_space, format_size};

use crate::apps::{AppReport, AppUsage};
use crate::{HomeRow, locations};

/// Action ids sent back by the home screen's buttons.
pub const GRANT_USAGE_ACCESS: &str = "grant-usage-access";
pub const REFRESH_APPS: &str = "refresh-apps";

const DRIVE: i32 = 0;
const HEADING: i32 = 1;
const BREAKDOWN: i32 = 2;
const APP: i32 = 3;
const NOTICE: i32 = 4;

/// What is known about app storage.
pub enum AppsState {
    /// The platform can't report it.
    Unsupported,
    /// The user hasn't allowed Usage access yet.
    NeedsAccess,
    Loading,
    Ready(AppReport),
    Failed(String),
}

pub fn rows(apps: &AppsState) -> Vec<HomeRow> {
    let mut rows = vec![heading("Choose a drive or folder to analyze", "", None)];
    rows.extend(locations::list().iter().map(drive_row));
    let title = "Apps & system";
    match apps {
        AppsState::Unsupported => {}
        AppsState::NeedsAccess => {
            rows.push(heading(title, "", None));
            rows.push(notice(
                "See what apps and Android are using",
                "Most of a phone's storage is app data and the system, which Android hides \
                 from file scans. Allow Usage access and DiskTree can show how much space \
                 each app takes.",
                Some(("Allow usage access", GRANT_USAGE_ACCESS)),
            ));
        }
        AppsState::Loading => {
            rows.push(heading(title, "", None));
            rows.push(notice("Measuring apps…", "", None));
        }
        AppsState::Failed(err) => {
            rows.push(heading(title, "", None));
            rows.push(notice(
                "Couldn't measure apps",
                err,
                Some(("Try again", REFRESH_APPS)),
            ));
        }
        AppsState::Ready(report) => {
            rows.push(heading(title, "", Some(("Refresh", REFRESH_APPS))));
            if let Some(space) = disk_space(&locations::default_path()) {
                rows.push(breakdown_row(report, space));
            }
            rows.push(heading(
                "Apps by size",
                "Tap an app to clear its cache or data, or uninstall it, in Android's settings.",
                None,
            ));
            let largest = report.apps.first().map_or(1, |a| a.size().max(1));
            rows.extend(
                report
                    .apps
                    .iter()
                    .filter(|app| app.size() > 0)
                    .map(|app| app_row(app, largest)),
            );
        }
    }
    rows
}

fn share(part: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (part as f64 / total as f64).clamp(0.0, 1.0) as f32
    }
}

fn drive_row(location: &locations::Location) -> HomeRow {
    let (detail, fraction) = match disk_space(&location.path) {
        Some(space) if space.total > 0 => (
            format!(
                "{} free of {}",
                format_size(space.free),
                format_size(space.total)
            ),
            share(space.used(), space.total),
        ),
        _ => (String::new(), 0.0),
    };
    let path = location.path.to_string_lossy();
    HomeRow {
        kind: DRIVE,
        title: location.label.as_str().into(),
        subtitle: path.as_ref().into(),
        detail: detail.into(),
        fraction,
        key: path.as_ref().into(),
        ..Default::default()
    }
}

fn heading(title: &str, text: &str, action: Option<(&str, &str)>) -> HomeRow {
    let (action, key) = action.unwrap_or_default();
    HomeRow {
        kind: HEADING,
        title: title.into(),
        subtitle: text.into(),
        action: action.into(),
        key: key.into(),
        ..Default::default()
    }
}

fn notice(title: &str, text: &str, action: Option<(&str, &str)>) -> HomeRow {
    let (action, key) = action.unwrap_or_default();
    HomeRow {
        kind: NOTICE,
        title: title.into(),
        subtitle: text.into(),
        action: action.into(),
        key: key.into(),
        ..Default::default()
    }
}

fn breakdown_row(report: &AppReport, space: DiskSpace) -> HomeRow {
    const GB: u64 = 1024 * 1024 * 1024;
    let b = report.breakdown(space);
    let mut notes = Vec::new();
    if b.cache > 0 {
        notes.push(format!(
            "{} of the apps' space is cache they can rebuild.",
            format_size(b.cache)
        ));
    }
    if b.outside >= GB {
        notes.push(format!(
            "Android itself takes about {} more, outside this space.",
            format_size(b.outside)
        ));
    }
    HomeRow {
        kind: BREAKDOWN,
        title: format!(
            "Internal storage: {} used of {}",
            format_size(b.total - b.free),
            format_size(b.total)
        )
        .into(),
        subtitle: notes.join(" ").into(),
        apps: share(b.apps, b.total),
        files: share(b.files, b.total),
        system: share(b.system, b.total),
        apps_label: format!("Apps {}", format_size(b.apps)).into(),
        files_label: format!("Your files {}", format_size(b.files)).into(),
        system_label: format!("System & other {}", format_size(b.system)).into(),
        free_label: format!("Free {}", format_size(b.free)).into(),
        ..Default::default()
    }
}

fn app_row(app: &AppUsage, largest: u64) -> HomeRow {
    let mut parts = Vec::new();
    if !app.system_image {
        parts.push(format!("App {}", format_size(app.code_bytes)));
    }
    parts.push(format!(
        "Data {}",
        format_size(app.data_bytes.saturating_sub(app.cache_bytes))
    ));
    if app.cache_bytes > 0 {
        parts.push(format!("Cache {}", format_size(app.cache_bytes)));
    }
    let title = if app.label.is_empty() {
        &app.package
    } else {
        &app.label
    };
    HomeRow {
        kind: APP,
        title: title.as_str().into(),
        subtitle: parts.join(" · ").into(),
        detail: format_size(app.size()).into(),
        fraction: share(app.size(), largest),
        key: app.package.as_str().into(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report() -> AppReport {
        AppReport {
            apps: vec![
                AppUsage {
                    package: "big.app".into(),
                    label: "Big".into(),
                    code_bytes: 100,
                    data_bytes: 900,
                    cache_bytes: 300,
                    system_image: false,
                },
                AppUsage {
                    package: "empty.app".into(),
                    label: String::new(),
                    code_bytes: 0,
                    data_bytes: 0,
                    cache_bytes: 0,
                    system_image: true,
                },
            ],
            shared_files: Some(500),
            advertised_total: None,
        }
    }

    #[test]
    fn unsupported_shows_only_drives() {
        let rows = rows(&AppsState::Unsupported);
        assert_eq!(rows[0].kind, HEADING);
        assert!(rows[1..].iter().all(|r| r.kind == DRIVE));
    }

    #[test]
    fn missing_access_offers_the_permission() {
        let rows = rows(&AppsState::NeedsAccess);
        let notice = rows.iter().find(|r| r.kind == NOTICE).unwrap();
        assert_eq!(notice.key, GRANT_USAGE_ACCESS);
    }

    #[test]
    fn ready_lists_apps_and_skips_empty_ones() {
        let rows = rows(&AppsState::Ready(report()));
        let apps: Vec<_> = rows.iter().filter(|r| r.kind == APP).collect();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].key, "big.app");
        assert_eq!(apps[0].subtitle, "App 100 B · Data 600 B · Cache 300 B");
        assert_eq!(apps[0].fraction, 1.0);
    }
}
