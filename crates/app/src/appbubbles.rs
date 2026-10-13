//! The start screen's apps as bubbles: one per app, sized the way the
//! selected group ranks them, and inside an app the parts its space is made
//! of.

use freesize_core::bubbles::{Circle, fit};
use freesize_core::{format_count, format_size};

use crate::apps::{AppReport, AppUsage};
use crate::insights::View;

/// Most apps drawn; the rest share one bubble.
const MAX_APPS: usize = 40;
/// Apps smaller than this share of the group join that bubble too.
const MIN_SHARE: f64 = 0.004;
const MB: u64 = 1024 * 1024;

/// What tapping a bubble does, by its key: an app's package zooms into it,
/// these do the rest.
pub const MORE: &str = "more";
/// `part:files:<package>` browses the files an app saved;
/// `part:app:<package>` opens the app's details.
pub const PART_PREFIX: &str = "part:";
pub const FILES: &str = "files";
pub const DETAILS: &str = "app";

/// Icon indexes of the UI's `Icons.of`.
const ICON_FOLDER: i32 = 0;
const ICON_PROGRAM: i32 = 9;
const ICON_DATABASE: i32 = 11;
const ICON_REBUILD: i32 = 12;
const ICON_MORE: i32 = 13;

#[derive(Clone, Debug, PartialEq)]
pub enum Icon {
    /// The app's own icon, by package.
    App(String),
    Kind(i32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppBubble {
    pub key: String,
    pub label: String,
    pub detail: String,
    pub circle: Circle,
    pub icon: Icon,
    /// Index for picking a color.
    pub color: u16,
}

/// The apps of `view` with what the group ranks them by, biggest first.
fn ranked(report: &AppReport, view: View) -> Vec<(&AppUsage, u64)> {
    let mut apps: Vec<(&AppUsage, u64)> = match view {
        View::Removed => Vec::new(),
        View::Media => report
            .media
            .iter()
            .filter(|media| media.bytes >= MB)
            .filter_map(|media| {
                report
                    .apps
                    .iter()
                    .find(|app| app.package == media.package)
                    .map(|app| (app, media.bytes))
            })
            .collect(),
        _ => report
            .apps
            .iter()
            .filter(|app| view.contains(app, report.usage_known, report.now))
            .map(|app| {
                let metric = match view {
                    View::Cache => app.cache_bytes,
                    View::Growing => app.growth.map_or(0, |g| g.bytes.max(0) as u64),
                    _ => app.size(),
                };
                (app, metric)
            })
            .filter(|&(_, metric)| metric > 0)
            .collect(),
    };
    apps.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.package.cmp(&b.0.package)));
    apps
}

fn name(app: &AppUsage) -> String {
    if app.label.is_empty() {
        app.package.clone()
    } else {
        app.label.clone()
    }
}

/// The bubbles for `view` in a `width` × `height` view, or the parts of the
/// app `zoom` when set.
pub fn bubbles(
    report: &AppReport,
    view: View,
    zoom: Option<&str>,
    width: f32,
    height: f32,
) -> Vec<AppBubble> {
    if let Some(app) = zoom.and_then(|p| report.apps.iter().find(|a| a.package == p)) {
        return parts(report, app, width, height);
    }
    let apps = ranked(report, view);
    let total: u64 = apps.iter().map(|&(_, metric)| metric).sum();
    let mut keep = apps
        .iter()
        .take(MAX_APPS)
        .take_while(|&&(_, metric)| metric as f64 >= total as f64 * MIN_SHARE)
        .count()
        .max(apps.len().min(1));
    if apps.len() == keep + 1 {
        keep += 1;
    }
    let rest = &apps[keep..];
    let mut sizes: Vec<u64> = apps[..keep].iter().map(|&(_, metric)| metric).collect();
    let rest_bytes: u64 = rest.iter().map(|&(_, metric)| metric).sum();
    if !rest.is_empty() {
        sizes.push(rest_bytes);
    }
    let circles = fit(&sizes, width, height, 8.0);
    let plus = if view == View::Growing { "+" } else { "" };
    let mut out: Vec<AppBubble> = apps[..keep]
        .iter()
        .zip(&circles)
        .enumerate()
        .map(|(i, (&(app, metric), &circle))| AppBubble {
            key: app.package.clone(),
            label: name(app),
            detail: format!("{plus}{}", format_size(metric)),
            circle,
            icon: Icon::App(app.package.clone()),
            color: i as u16,
        })
        .collect();
    if let Some(&circle) = circles.get(keep) {
        out.push(AppBubble {
            key: MORE.into(),
            label: format!("{} more", format_count(rest.len() as u64)),
            detail: format_size(rest_bytes),
            circle,
            icon: Icon::Kind(ICON_MORE),
            color: u16::MAX,
        });
    }
    out
}

/// What an app's space is made of: its code, data, cache and the files it
/// saved to shared storage.
fn parts(report: &AppReport, app: &AppUsage, width: f32, height: f32) -> Vec<AppBubble> {
    let package = &app.package;
    let media = report
        .media
        .iter()
        .find(|m| &m.package == package && m.bytes >= MB);
    // Files inside the app's data folder are part of its data size already.
    let media_in_data = media.filter(|m| m.in_app_data).map_or(0, |m| m.bytes);
    let code = if app.system_image || app.archived {
        0
    } else {
        app.code_bytes
    };
    let data = app
        .data_bytes
        .saturating_sub(app.cache_bytes)
        .saturating_sub(media_in_data);
    let details = format!("{PART_PREFIX}{DETAILS}:{package}");
    let mut parts = vec![
        ("App".to_string(), code, ICON_PROGRAM, details.clone()),
        ("Data".to_string(), data, ICON_DATABASE, details.clone()),
        ("Cache".to_string(), app.cache_bytes, ICON_REBUILD, details),
    ];
    if let Some(media) = media {
        parts.push((
            "Saved files".to_string(),
            media.bytes,
            ICON_FOLDER,
            format!("{PART_PREFIX}{FILES}:{package}"),
        ));
    }
    parts.retain(|&(_, bytes, _, _)| bytes > 0);
    let sizes: Vec<u64> = parts.iter().map(|&(_, bytes, _, _)| bytes).collect();
    let circles = fit(&sizes, width, height, 8.0);
    parts
        .into_iter()
        .zip(circles)
        .enumerate()
        .map(|(i, ((label, bytes, icon, key), circle))| AppBubble {
            key,
            label,
            detail: format_size(bytes),
            circle,
            icon: Icon::Kind(icon),
            color: i as u16,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::cleanup::MediaUse;

    fn app(package: &str, code: u64, data: u64, cache: u64) -> AppUsage {
        AppUsage {
            package: package.into(),
            label: package.to_uppercase(),
            code_bytes: code * MB,
            data_bytes: data * MB,
            cache_bytes: cache * MB,
            has_launcher: true,
            ..AppUsage::default()
        }
    }

    fn report() -> AppReport {
        let mut apps = vec![app("big", 100, 900, 300), app("mid", 50, 250, 0)];
        apps.extend((0..60).map(|i| app(&format!("tiny{i:02}"), 1, 0, 0)));
        AppReport {
            installed: apps.iter().map(|a| a.package.clone()).collect(),
            apps,
            media: vec![MediaUse {
                package: "big".into(),
                bytes: 200 * MB,
                files: 10,
                folder: PathBuf::from("/s/Android/media/big"),
                in_app_data: true,
            }],
            ..AppReport::default()
        }
    }

    #[test]
    fn apps_sized_by_the_group_with_the_rest_folded() {
        let report = report();
        let all = bubbles(&report, View::All, None, 400.0, 400.0);
        assert_eq!(all[0].key, "big");
        assert_eq!(all[0].icon, Icon::App("big".into()));
        // Apps under 0.4 % of the total share one bubble.
        let more = all.last().unwrap();
        assert_eq!(more.key, MORE);
        assert_eq!(more.label, "60 more");
        let cache = bubbles(&report, View::Cache, None, 400.0, 400.0);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache[0].detail, "300 MB");
        assert!(bubbles(&report, View::Removed, None, 400.0, 400.0).is_empty());
    }

    #[test]
    fn zooming_shows_the_parts_of_an_app() {
        let parts = bubbles(&report(), View::All, Some("big"), 400.0, 400.0);
        let labels: Vec<(&str, &str)> = parts
            .iter()
            .map(|p| (p.label.as_str(), p.detail.as_str()))
            .collect();
        // Data leaves out the cache and the saved files counted in it.
        assert_eq!(
            labels,
            [
                ("App", "100 MB"),
                ("Data", "400 MB"),
                ("Cache", "300 MB"),
                ("Saved files", "200 MB")
            ]
        );
        assert_eq!(parts[3].key, "part:files:big");
        assert_eq!(parts[0].key, "part:app:big");
    }
}
