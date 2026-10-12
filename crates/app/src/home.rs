//! Rows of the home screen: drives and folders to scan and, where the
//! platform reports it, how the device's storage divides up between apps,
//! the user's files and the system.

use disktree_core::{DiskSpace, disk_space, format_size};

use std::path::Path;

use crate::apps::{Abilities, AppReport, AppUsage};
use crate::cleanup::{Junk, JunkKind, MediaUse, SAVED_APKS};
use crate::growth::{self, DriveLog};
use crate::history::Entry;
use crate::insights::{self, Source, View, ago, date, duration};
use crate::{HomeRow, locations};

/// Action ids sent back by the home screen's buttons.
pub const GRANT_USAGE_ACCESS: &str = "grant-usage-access";
pub const REFRESH_APPS: &str = "refresh-apps";
pub const OPEN_APP_INFO: &str = "open-app-info";
pub const CLEAR_CACHE: &str = "clear-cache";
pub const GRANT_STORAGE: &str = "grant-storage";
/// Prefix of the action ids that switch the app list to another group.
pub const VIEW_PREFIX: &str = "view:";
/// Prefix of the keys of removed apps' rows; installed apps use the bare
/// package name.
pub const REMOVED_PREFIX: &str = "removed:";
/// Prefix of the action ids that delete a kind of junk (see `JunkKind::id`).
pub const CLEAN_PREFIX: &str = "clean:";

/// Junk and app media smaller than this isn't worth a row.
const WORTH_SHOWING: u64 = 1024 * 1024;
/// Media from this size on is mentioned in an app's tags.
const MEDIA_TAG: u64 = 100 * 1024 * 1024;

/// Action ids of the app sheet's buttons.
pub mod sheet {
    pub const UNINSTALL: &str = "uninstall";
    pub const ARCHIVE: &str = "archive";
    pub const SAVE_APK: &str = "save-apk";
    pub const BROWSE: &str = "browse";
    pub const STORE: &str = "store";
    pub const SEARCH_STORE: &str = "search-store";
    pub const APP_INFO: &str = "app-info";
    pub const FORGET: &str = "forget";
    pub const CLOSE: &str = "close";
}

const DRIVE: i32 = 0;
const HEADING: i32 = 1;
const BREAKDOWN: i32 = 2;
const APP: i32 = 3;
const NOTICE: i32 = 4;
const GROUP: i32 = 5;
const CHART: i32 = 6;

/// Android keeps roughly this much usage history.
const USAGE_HISTORY: u64 = 2 * 365 * 24 * 60 * 60;

/// What is known about app storage.
pub enum AppsState {
    /// The platform can't report it.
    Unsupported,
    /// The user hasn't allowed Usage access yet; `asked` once they have
    /// been sent to the settings screen at least once.
    NeedsAccess {
        asked: bool,
    },
    Loading,
    Ready(AppReport),
    Failed(String),
}

/// Everything besides the app data that changes the home screen.
#[derive(Clone, Copy, Debug, Default)]
pub struct Options<'a> {
    /// Which group of apps the list shows.
    pub view: View,
    /// A new measurement is running; the previous one stays on screen.
    pub refreshing: bool,
    pub storage_access: bool,
    pub abilities: Abilities,
    /// What Android's clear-cache dialog freed the last time it was used.
    pub cache_freed: Option<u64>,
    /// Past sizes of the drives, for their trends.
    pub drives: Option<&'a DriveLog>,
}

pub fn rows(apps: &AppsState, options: Options<'_>) -> Vec<HomeRow> {
    let mut rows = vec![heading("Choose a drive or folder to analyze", "", None)];
    rows.extend(
        locations::list()
            .iter()
            .map(|location| drive_row(location, options.drives)),
    );
    let title = "Apps & system";
    match apps {
        AppsState::Unsupported => {}
        AppsState::NeedsAccess { asked } => {
            rows.push(heading(title, "", None));
            rows.push(notice(
                "See what apps and Android are using",
                "Most of a phone's storage is app data and the system, which Android hides \
                 from file scans. Allow Usage access and DiskTree can show how much space \
                 each app takes.",
                Some(("Allow usage access", GRANT_USAGE_ACCESS)),
            ));
            if *asked {
                rows.push(notice(
                    "Switch greyed out, or “App was denied access”?",
                    "Android blocks Usage access for apps installed from a downloaded file \
                     until you allow it. Open DiskTree's app info, tap ⋮ in the top corner, \
                     choose “Allow restricted settings” and confirm. Then tap Allow usage \
                     access again.",
                    Some(("Open DiskTree's app info", OPEN_APP_INFO)),
                ));
            }
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
            if options.refreshing {
                rows.push(heading(title, "Updating…", None));
            } else {
                rows.push(heading(title, "", Some(("Refresh", REFRESH_APPS))));
            }
            if let Some(space) = disk_space(&locations::default_path()) {
                rows.push(breakdown_row(report, space));
            }
            rows.extend(chart_row(report));
            rows.extend(free_up_rows(report, options));
            rows.extend(
                View::ALL
                    .into_iter()
                    .filter_map(|view| group_row(report, view, options.view)),
            );
            let view = options.view;
            rows.push(heading(view.title(), &explanation(view, options), None));
            let before = rows.len();
            if view == View::Removed {
                rows.extend(report.removed.iter().map(|e| removed_row(e, report.now)));
            } else if view == View::Cache {
                let mut apps: Vec<&AppUsage> = report
                    .apps
                    .iter()
                    .filter(|app| in_view(report, app, view))
                    .collect();
                apps.sort_by_key(|app| std::cmp::Reverse(app.cache_bytes));
                let largest = apps.first().map_or(1, |a| a.cache_bytes.max(1));
                rows.extend(apps.into_iter().map(|app| {
                    let mut row = app_row(app, largest, report);
                    row.detail = format_size(app.cache_bytes).into();
                    row.fraction = share(app.cache_bytes, largest);
                    row
                }));
            } else if view == View::Growing {
                let mut apps: Vec<(&AppUsage, u64)> = report
                    .apps
                    .iter()
                    .filter(|app| in_view(report, app, view))
                    .map(|app| (app, grown(app)))
                    .collect();
                apps.sort_by_key(|&(_, grew)| std::cmp::Reverse(grew));
                let largest = apps.first().map_or(1, |&(_, grew)| grew.max(1));
                rows.extend(apps.into_iter().map(|(app, grew)| {
                    let mut row = app_row(app, largest, report);
                    row.detail = format!("+{}", format_size(grew)).into();
                    row.fraction = share(grew, largest);
                    row
                }));
            } else if view == View::Media {
                let media: Vec<(&MediaUse, &AppUsage)> = media_apps(report).collect();
                let largest = media.first().map_or(1, |(m, _)| m.bytes.max(1));
                rows.extend(
                    media
                        .into_iter()
                        .map(|(media, app)| media_row(media, app, largest, report)),
                );
            } else {
                let apps: Vec<&AppUsage> = report
                    .apps
                    .iter()
                    .filter(|app| app.size() > 0 && in_view(report, app, view))
                    .collect();
                let largest = apps.first().map_or(1, |a| a.size().max(1));
                rows.extend(apps.into_iter().map(|app| app_row(app, largest, report)));
            }
            if rows.len() == before {
                rows.push(notice("Nothing left in this group.", "", None));
            }
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

fn drive_row(location: &locations::Location, drives: Option<&DriveLog>) -> HomeRow {
    let path = location.path.to_string_lossy();
    let space = disk_space(&location.path).filter(|space| space.total > 0);
    let (detail, fraction) = match space {
        Some(space) => (
            format!(
                "{} free of {}",
                format_size(space.free),
                format_size(space.total)
            ),
            share(space.used(), space.total),
        ),
        None => (String::new(), 0.0),
    };
    let (tags, warn) = space
        .zip(drives)
        .and_then(|(space, drives)| drives.trend(&path, space.free))
        .unwrap_or_default();
    HomeRow {
        kind: DRIVE,
        title: location.label.as_str().into(),
        subtitle: path.as_ref().into(),
        detail: detail.into(),
        fraction,
        key: path.as_ref().into(),
        tags: tags.into(),
        warn,
        ..Default::default()
    }
}

/// How much an app grew over the last month; 0 if it didn't.
fn grown(app: &AppUsage) -> u64 {
    app.growth.map_or(0, |g| g.bytes.max(0) as u64)
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

fn in_view(report: &AppReport, app: &AppUsage, view: View) -> bool {
    view.contains(app, report.usage_known, report.now)
}

/// The "Free up space" section: clearing every app's cache, the trash, old
/// installers, the thumbnail cache and leftovers of removed apps.
fn free_up_rows(report: &AppReport, options: Options<'_>) -> Vec<HomeRow> {
    const CACHE_WORTH_IT: u64 = 50 * 1024 * 1024;
    let title = "Free up space";
    if !options.storage_access {
        return vec![
            heading(title, "", None),
            notice(
                "Find the trash, old installers and more",
                "DiskTree needs All files access to look through your files for space you \
                 can free, and to clear every app's cache.",
                Some(("Grant access", GRANT_STORAGE)),
            ),
        ];
    }
    let mut cards = Vec::new();
    let cache: u64 = report.apps.iter().map(|a| a.cache_bytes).sum();
    if options.abilities.clear_cache && cache >= CACHE_WORTH_IT {
        let text = match options.cache_freed {
            // Android decides how much to clear and may leave most of it.
            Some(freed) if freed < cache / 10 => format!(
                "Android's cache cleaner only freed {} last time: it decides how much to \
                 clear. To clear more, open the apps under Biggest caches below and use \
                 Clear cache on their info pages.",
                format_size(freed)
            ),
            Some(freed) => format!(
                "Android cleared {} last time. Apps rebuild some cache as they run.",
                format_size(freed)
            ),
            None => "Android's own cache cleaner; it asks you to confirm first and decides \
                     how much to clear. Apps rebuild their cache as they need it."
                .into(),
        };
        cards.push(notice(
            &format!("Clear every app's cache: {}", format_size(cache)),
            &text,
            Some(("Clear cache", CLEAR_CACHE)),
        ));
    }
    for kind in JunkKind::ALL {
        if let Some(junk) = report
            .junk
            .iter()
            .find(|j| j.kind == kind && j.bytes >= WORTH_SHOWING)
        {
            cards.push(junk_row(junk));
        }
    }
    if cards.is_empty() {
        return cards;
    }
    let mut rows = vec![heading(title, "", None)];
    rows.extend(cards);
    rows
}

/// "a.jpg, b.jpg and 3 more".
fn some_names(names: &[String]) -> String {
    const SHOWN: usize = 3;
    let shown = names
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().saturating_sub(SHOWN) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn junk_row(junk: &Junk) -> HomeRow {
    let size = format_size(junk.bytes);
    let n = junk.paths.len();
    let (title, text, button) = match junk.kind {
        JunkKind::Trash => (
            format!("Empty the trash: {size}"),
            format!(
                "{} you deleted in the last 30 days, such as {}. They still take space until \
                 Android removes them for good.",
                count(n, "item", "items"),
                some_names(&junk.names)
            ),
            "Empty trash",
        ),
        JunkKind::Installers => (
            format!("Delete old app installers: {size}"),
            format!(
                "{} in Download: {}. Installers you saved with DiskTree in {SAVED_APKS} are \
                 kept.",
                count(n, "APK file", "APK files"),
                some_names(&junk.names)
            ),
            "Delete",
        ),
        JunkKind::Thumbnails => (
            format!("Clear the thumbnail cache: {size}"),
            "Old preview images in DCIM/.thumbnails. Gallery apps make new ones when they \
             need them."
                .to_string(),
            "Clear",
        ),
        JunkKind::Leftovers => (
            format!("Delete leftovers of removed apps: {size}"),
            format!(
                "{} in Android/media from apps that are no longer installed: {}.",
                count(n, "folder", "folders"),
                some_names(&junk.names)
            ),
            "Delete",
        ),
    };
    let key = format!("{CLEAN_PREFIX}{}", junk.kind.id());
    notice(&title, &text, Some((button, &key)))
}

/// The question asked before deleting a kind of junk.
pub fn junk_confirm(junk: &Junk) -> String {
    let size = format_size(junk.bytes);
    let n = junk.paths.len();
    match junk.kind {
        JunkKind::Trash => format!(
            "Permanently delete {} from the trash ({size})? They can't be restored afterwards.",
            count(n, "item", "items")
        ),
        JunkKind::Installers => format!(
            "Permanently delete {} from Download ({size})?",
            count(n, "APK file", "APK files")
        ),
        JunkKind::Thumbnails => {
            format!("Delete the thumbnail cache ({size})? Gallery apps rebuild it as needed.")
        }
        JunkKind::Leftovers => format!(
            "Permanently delete {} left by removed apps ({size})? Their files can't be \
             restored.",
            count(n, "folder", "folders")
        ),
    }
}

/// Media owners that are installed apps, with enough media to list.
fn media_apps(report: &AppReport) -> impl Iterator<Item = (&MediaUse, &AppUsage)> {
    report
        .media
        .iter()
        .filter(|media| media.bytes >= WORTH_SHOWING)
        .filter_map(|media| {
            let app = report
                .apps
                .iter()
                .find(|app| app.package == media.package)?;
            Some((media, app))
        })
}

/// `path` relative to the storage root when it's inside it.
fn folder_name(path: &Path) -> String {
    let root = locations::default_path();
    path.strip_prefix(&root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn media_row(media: &MediaUse, app: &AppUsage, largest: u64, report: &AppReport) -> HomeRow {
    let title = if app.label.is_empty() {
        &app.package
    } else {
        &app.label
    };
    HomeRow {
        kind: APP,
        title: title.as_str().into(),
        subtitle: format!(
            "{}, mostly in {}",
            count(media.files as usize, "file", "files"),
            folder_name(&media.folder)
        )
        .into(),
        tags: tags(app, report).into(),
        warn: hard_to_get_back(app),
        detail: format_size(media.bytes).into(),
        fraction: share(media.bytes, largest),
        key: app.package.as_str().into(),
        ..Default::default()
    }
}

/// A row that switches the list to `view`. Groups with nothing in them are
/// left out, unless selected.
fn group_row(report: &AppReport, view: View, selected: View) -> Option<HomeRow> {
    let (count, bytes) = if view == View::Removed {
        (report.removed.len(), 0)
    } else if view == View::Media {
        media_apps(report).fold((0, 0), |(n, b), (media, _)| (n + 1, b + media.bytes))
    } else if view == View::Cache {
        report
            .apps
            .iter()
            .filter(|app| in_view(report, app, view))
            .fold((0, 0), |(n, b), app| (n + 1, b + app.cache_bytes))
    } else if view == View::Growing {
        report
            .apps
            .iter()
            .filter(|app| in_view(report, app, view))
            .fold((0, 0), |(n, b), app| (n + 1, b + grown(app)))
    } else {
        report
            .apps
            .iter()
            .filter(|app| app.size() > 0 && in_view(report, app, view))
            .fold((0, 0), |(n, b), app| (n + 1, b + app.size()))
    };
    if count == 0 && view != View::All && view != selected {
        return None;
    }
    let apps = if count == 1 { "app" } else { "apps" };
    let detail = if bytes > 0 && view == View::Growing {
        format!("{count} {apps} · +{}", format_size(bytes))
    } else if bytes > 0 {
        format!("{count} {apps} · {}", format_size(bytes))
    } else {
        format!("{count} {apps}")
    };
    Some(HomeRow {
        kind: GROUP,
        title: view.title().into(),
        detail: detail.into(),
        key: format!("{VIEW_PREFIX}{}", view.id()).into(),
        selected: view == selected,
        ..Default::default()
    })
}

fn explanation(view: View, options: Options<'_>) -> String {
    match view {
        View::All => "Biggest first. Tap an app to see when you last used it, and to \
                      uninstall it, archive it or clear its cache."
            .into(),
        View::Unused => {
            let mut text = String::from(
                "Apps you haven't opened in 3 months, biggest first. System apps, default \
                 apps such as your keyboard, and apps without an icon are left out.",
            );
            if options.abilities.archive {
                text.push_str(
                    " Archiving a Play Store app frees its app size but keeps its data; \
                     uninstalling frees both.",
                );
            }
            text
        }
        View::NeverOpened => "No use recorded since they were installed. Android keeps \
                              about 2 years of usage history."
            .into(),
        View::NotFromStore => "Installed from a file or from a computer, so no app store \
                               can give them back. Save an app's APK before uninstalling it \
                               if you might want it again."
            .into(),
        View::Media => "Photos, videos, downloads and other files each app saved to your \
                        storage, biggest first. Tap an app to browse its files and delete \
                        what you don't need."
            .into(),
        View::Cache => "Cache is temporary data apps rebuild when they need it, so it's \
                        safe to clear. Tap an app, choose App info, then Storage & cache → \
                        Clear cache."
            .into(),
        View::Growing => "Apps that grew by 100 MB or more over the last month, fastest \
                          first. It's usually downloads, offline music and videos, or cache \
                          piling up. DiskTree measures apps each day it's opened."
            .into(),
        View::Removed => "Apps DiskTree has seen that are no longer installed. Tap one to \
                          find it again."
            .into(),
    }
}

/// Where the app came from and when it was last used, for the list.
fn tags(app: &AppUsage, report: &AppReport) -> String {
    let mut tags = vec![Source::of(app).tag().to_string()];
    if app.archived {
        tags.push("Archived".into());
    } else if let Some(reason) = &app.protected {
        if !app.own {
            tags.push(capitalized(reason));
        }
    } else if report.usage_known && !app.system {
        tags.push(match app.last_opened {
            Some(t) => format!("opened {}", ago(report.now, t)),
            None => never_opened(app, report.now).into(),
        });
    }
    if let Some(growth) = app.growth.filter(|_| insights::growing(app)) {
        tags.push(format!(
            "+{} in {} days",
            format_size(growth.bytes.unsigned_abs()),
            growth.days
        ));
    }
    if let Some(media) = media_of(report, &app.package).filter(|m| m.bytes >= MEDIA_TAG) {
        tags.push(format!("{} of files", format_size(media.bytes)));
    }
    tags.join(" · ")
}

fn media_of<'a>(report: &'a AppReport, package: &str) -> Option<&'a MediaUse> {
    report.media.iter().find(|media| media.package == package)
}

fn never_opened(app: &AppUsage, now: u64) -> &'static str {
    match app.installed_at {
        Some(t) if now.saturating_sub(t) < USAGE_HISTORY => "never opened",
        _ => "not opened in 2 years",
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// Can't be downloaded again: highlighted so it isn't removed by accident.
fn hard_to_get_back(app: &AppUsage) -> bool {
    !app.system && !app.own && !Source::of(app).reinstallable()
}

fn removed_row(entry: &Entry, now: u64) -> HomeRow {
    let app = entry.as_app();
    let title = if entry.label.is_empty() {
        &entry.package
    } else {
        &entry.label
    };
    let removed = entry.removed_at.map_or_else(String::new, |t| ago(now, t));
    HomeRow {
        kind: APP,
        title: title.as_str().into(),
        subtitle: format!("Removed {removed} · was {}", format_size(entry.size)).into(),
        tags: Source::of(&app).tag().into(),
        warn: hard_to_get_back(&app),
        key: format!("{REMOVED_PREFIX}{}", entry.package).into(),
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

/// Storage over time, once there are samples from two days.
fn chart_row(report: &AppReport) -> Option<HomeRow> {
    let chart = growth::chart(&report.storage)?;
    Some(HomeRow {
        kind: CHART,
        title: "Storage over time".into(),
        subtitle: growth::storage_summary(&report.storage).into(),
        chart_used: chart.used.into(),
        chart_files: chart.files.into(),
        chart_apps: chart.apps.into(),
        chart_start: chart.start.into(),
        chart_end: chart.end.into(),
        ..Default::default()
    })
}

fn app_row(app: &AppUsage, largest: u64, report: &AppReport) -> HomeRow {
    let mut parts = Vec::new();
    if !app.system_image && !app.archived {
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
        tags: tags(app, report).into(),
        warn: hard_to_get_back(app),
        detail: format_size(app.size()).into(),
        fraction: share(app.size(), largest),
        key: app.package.as_str().into(),
        ..Default::default()
    }
}

/// The details and actions shown when an app is tapped.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sheet {
    pub title: String,
    pub subtitle: String,
    pub facts: Vec<String>,
    /// Advice to read first, highlighted; may be empty.
    pub note: String,
    /// (action id, button text, primary)
    pub actions: Vec<(&'static str, String, bool)>,
}

fn source_phrase(app: &AppUsage) -> String {
    match Source::of(app) {
        Source::Play => "the Play Store".into(),
        Source::Store(name) => name.into(),
        Source::Downloaded => match &app.installer_label {
            Some(via) => format!("a downloaded file (via {via})"),
            None => "a downloaded file".into(),
        },
        Source::Preinstalled => "the phone's software".into(),
        Source::Unknown => "an unknown source, probably a computer".into(),
    }
}

pub fn app_sheet(app: &AppUsage, report: &AppReport, abilities: Abilities) -> Sheet {
    let now = report.now;
    let source = Source::of(app);
    let mut facts = Vec::new();

    if app.archived {
        facts.push(format!(
            "Archived: the app was removed and its {} of data kept. Tap its icon to get it \
             back from the Play Store.",
            format_size(app.data_bytes)
        ));
    } else {
        facts.push(format!(
            "Uses {}: app {}, data {}, cache {}.",
            format_size(app.size()),
            format_size(if app.system_image { 0 } else { app.code_bytes }),
            format_size(app.data_bytes.saturating_sub(app.cache_bytes)),
            format_size(app.cache_bytes),
        ));
    }
    if report.usage_known {
        facts.push(match app.last_opened {
            Some(t) => format!("Last opened {} ({}).", ago(now, t), date(t)),
            None if never_opened(app, now) == "never opened" => {
                "Never opened since it was installed.".into()
            }
            None => "Not opened in the last 2 years, as far back as Android keeps track.".into(),
        });
        facts.push(match app.screen_time {
            Some(secs) if secs > 0 => {
                format!("On screen {} in the last month.", duration(secs))
            }
            _ => "Not on screen in the last month.".into(),
        });
    }
    if source == Source::Preinstalled {
        facts.push("Came with the phone.".into());
    } else {
        facts.push(match app.installed_at {
            Some(t) => format!("Installed {} from {}.", date(t), source_phrase(app)),
            None => format!("Installed from {}.", source_phrase(app)),
        });
    }
    let media = media_of(report, &app.package).filter(|m| m.bytes >= WORTH_SHOWING);
    if let Some(media) = media {
        facts.push(if media.in_app_data {
            format!(
                "{} of its data is files in {}, which you can browse and delete.",
                format_size(media.bytes),
                folder_name(&media.folder)
            )
        } else {
            format!(
                "Saved {} to your storage ({}), mostly in {}.",
                format_size(media.bytes),
                count(media.files as usize, "file", "files"),
                folder_name(&media.folder)
            )
        });
    }
    if let Some(growth) = app
        .growth
        .filter(|g| g.bytes.unsigned_abs() >= WORTH_SHOWING)
    {
        let verb = if growth.bytes < 0 { "Shrank" } else { "Grew" };
        facts.push(format!(
            "{verb} {} over the last {} days.",
            format_size(growth.bytes.unsigned_abs()),
            growth.days
        ));
    }
    if let Some(category) = insights::category(app.category) {
        facts.push(format!("Category: {category}."));
    }
    if let Some(reason) = app.protected.as_ref().filter(|_| !app.own) {
        facts.push(format!("Works without being opened: it's {reason}."));
    }
    facts.push(match source {
        _ if app.own => "This is DiskTree.".into(),
        Source::Play => "If you uninstall it, the Play Store has it.".into(),
        Source::Store(name) => format!("If you uninstall it, {name} has it."),
        Source::Downloaded | Source::Unknown => "No app store has it. Save its APK before \
                                                 uninstalling if you might want it again."
            .into(),
        Source::Preinstalled => "Part of Android or the phone maker's software, so it can't \
                                 be uninstalled. Its info page can clear its cache, and \
                                 sometimes disable it."
            .into(),
    });

    let mut actions = Vec::new();
    let removable = !app.system && !app.own;
    if removable && !source.reinstallable() && !app.archived && abilities.save_apk {
        actions.push((sheet::SAVE_APK, "Save APK to Downloads".into(), true));
    }
    if removable && abilities.uninstall {
        actions.push((sheet::UNINSTALL, "Uninstall".into(), source.reinstallable()));
    }
    if removable && abilities.archive && source == Source::Play && !app.archived {
        actions.push((sheet::ARCHIVE, "Archive (keeps its data)".into(), false));
    }
    if media.is_some() {
        actions.push((sheet::BROWSE, "Browse its files".into(), false));
    }
    if source.reinstallable() {
        actions.push((sheet::STORE, format!("Open in {}", source.tag()), false));
    }
    actions.push((
        sheet::APP_INFO,
        "App info: clear cache or data".into(),
        false,
    ));
    actions.push((sheet::CLOSE, "Close".into(), false));

    Sheet {
        title: if app.label.is_empty() {
            app.package.clone()
        } else {
            app.label.clone()
        },
        subtitle: if app.version.is_empty() {
            app.package.clone()
        } else {
            format!("{} · version {}", app.package, app.version)
        },
        facts,
        note: insights::tip(&app.package).unwrap_or_default().into(),
        actions,
    }
}

pub fn removed_sheet(entry: &Entry, now: u64) -> Sheet {
    let app = entry.as_app();
    let source = Source::of(&app);
    let mut facts = Vec::new();
    if let Some(t) = entry.removed_at {
        facts.push(format!(
            "Removed {}: DiskTree noticed it was gone on {}.",
            ago(now, t),
            date(t)
        ));
    }
    let version = if entry.version.is_empty() {
        String::new()
    } else {
        format!(", version {}", entry.version)
    };
    facts.push(format!(
        "Used {} when last seen{version}.",
        format_size(entry.size)
    ));
    if let Some(t) = entry.installed_at {
        facts.push(format!(
            "Installed {} from {}.",
            date(t),
            source_phrase(&app)
        ));
    }
    let mut actions = Vec::new();
    if source.reinstallable() {
        facts.push(format!(
            "{} should still have it, unless its developer took it down.",
            capitalized(&source_phrase(&app))
        ));
        actions.push((sheet::STORE, format!("Open in {}", source.tag()), true));
    } else {
        facts.push(
            "It wasn't from an app store. Look for the APK where you got it, or in \
             Download/DiskTree if you saved it there."
                .into(),
        );
        actions.push((sheet::SEARCH_STORE, "Search the Play Store".into(), false));
    }
    actions.push((sheet::FORGET, "Remove from this list".into(), false));
    actions.push((sheet::CLOSE, "Close".into(), false));
    Sheet {
        title: if entry.label.is_empty() {
            entry.package.clone()
        } else {
            entry.label.clone()
        },
        subtitle: entry.package.clone(),
        facts,
        note: String::new(),
        actions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 24 * 60 * 60;
    const NOW: u64 = 1_790_000_000;

    fn app(package: &str, code: u64, data: u64, cache: u64, opened_days_ago: u64) -> AppUsage {
        AppUsage {
            package: package.into(),
            label: package.to_uppercase(),
            code_bytes: code,
            data_bytes: data,
            cache_bytes: cache,
            installer: Some("com.android.vending".into()),
            installed_at: Some(NOW - 500 * DAY),
            last_opened: Some(NOW - opened_days_ago * DAY),
            has_launcher: true,
            category: -1,
            ..AppUsage::default()
        }
    }

    fn report() -> AppReport {
        let mut empty = app("empty.app", 0, 0, 0, 0);
        empty.system = true;
        empty.system_image = true;
        let mut tool = app("tool.app", 10, 20, 0, 400);
        tool.installer = Some("com.google.android.packageinstaller".into());
        AppReport {
            apps: vec![app("big.app", 100, 900, 300, 1), tool, empty],
            installed: vec!["big.app".into(), "tool.app".into(), "empty.app".into()],
            shared_files: Some(500),
            usage_known: true,
            removed: vec![Entry {
                package: "gone.app".into(),
                label: "Gone".into(),
                installer: Some("com.android.vending".into()),
                removed_at: Some(NOW - 10 * DAY),
                size: 2048,
                ..Entry::default()
            }],
            now: NOW,
            ..AppReport::default()
        }
    }

    fn options(view: View) -> Options<'static> {
        Options {
            view,
            storage_access: true,
            abilities: Abilities {
                uninstall: true,
                archive: true,
                clear_cache: true,
                save_apk: true,
            },
            ..Options::default()
        }
    }

    fn of_kind(rows: &[HomeRow], kind: i32) -> Vec<&HomeRow> {
        rows.iter().filter(|r| r.kind == kind).collect()
    }

    #[test]
    fn unsupported_shows_only_drives() {
        let rows = rows(&AppsState::Unsupported, Options::default());
        assert_eq!(rows[0].kind, HEADING);
        assert!(rows[1..].iter().all(|r| r.kind == DRIVE));
    }

    #[test]
    fn missing_access_offers_the_permission() {
        let rows = rows(&AppsState::NeedsAccess { asked: false }, Options::default());
        let notices = of_kind(&rows, NOTICE);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].key, GRANT_USAGE_ACCESS);
    }

    #[test]
    fn after_asking_explains_restricted_settings() {
        let rows = rows(&AppsState::NeedsAccess { asked: true }, Options::default());
        let keys: Vec<_> = of_kind(&rows, NOTICE)
            .iter()
            .map(|r| r.key.as_str())
            .collect();
        assert_eq!(keys, [GRANT_USAGE_ACCESS, OPEN_APP_INFO]);
    }

    #[test]
    fn all_apps_lists_by_size_and_skips_empty_ones() {
        let rows = rows(&AppsState::Ready(report()), options(View::All));
        let apps = of_kind(&rows, APP);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].key, "big.app");
        assert_eq!(apps[0].subtitle, "App 100 B · Data 600 B · Cache 300 B");
        assert_eq!(apps[0].tags, "Play Store · opened yesterday");
        assert!(!apps[0].warn);
        assert_eq!(apps[0].fraction, 1.0);
        assert_eq!(apps[1].tags, "Downloaded file · opened 13 months ago");
        assert!(apps[1].warn);
    }

    #[test]
    fn groups_show_counts_and_switch_the_list() {
        let rows = rows(&AppsState::Ready(report()), options(View::Unused));
        let groups: Vec<_> = of_kind(&rows, GROUP)
            .iter()
            .map(|r| (r.title.to_string(), r.detail.to_string(), r.selected))
            .collect();
        assert_eq!(
            groups,
            [
                ("All apps".into(), "2 apps · 1.01 KB".into(), false),
                ("Not opened in 3 months".into(), "1 app · 30 B".into(), true),
                ("Not from an app store".into(), "1 app · 30 B".into(), false),
                ("Removed apps".into(), "1 app".into(), false),
            ]
        );
        let apps = of_kind(&rows, APP);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].key, "tool.app");
    }

    #[test]
    fn removed_view_lists_history() {
        let rows = rows(&AppsState::Ready(report()), options(View::Removed));
        let apps = of_kind(&rows, APP);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].key, "removed:gone.app");
        assert_eq!(apps[0].subtitle, "Removed 10 days ago · was 2.00 KB");
    }

    #[test]
    fn clear_cache_needs_the_ability_and_enough_cache() {
        let mut report = report();
        let rows_now = rows(&AppsState::Ready(report.clone()), options(View::All));
        assert!(!rows_now.iter().any(|r| r.key == CLEAR_CACHE));
        report.apps[0].cache_bytes = 100 * 1024 * 1024;
        report.apps[0].data_bytes = 200 * 1024 * 1024;
        let rows_now = rows(&AppsState::Ready(report.clone()), options(View::All));
        assert!(rows_now.iter().any(|r| r.key == CLEAR_CACHE));
        let mut no_access = options(View::All);
        no_access.storage_access = false;
        let rows_now = rows(&AppsState::Ready(report), no_access);
        assert!(rows_now.iter().any(|r| r.key == GRANT_STORAGE));
    }

    #[test]
    fn sheet_offers_saving_apps_no_store_has() {
        let report = report();
        let abilities = options(View::All).abilities;
        let ids = |app: &AppUsage| -> Vec<&str> {
            app_sheet(app, &report, abilities)
                .actions
                .iter()
                .map(|a| a.0)
                .collect()
        };
        assert_eq!(
            ids(&report.apps[0]),
            [
                sheet::UNINSTALL,
                sheet::ARCHIVE,
                sheet::STORE,
                sheet::APP_INFO,
                sheet::CLOSE
            ]
        );
        assert_eq!(
            ids(&report.apps[1]),
            [
                sheet::SAVE_APK,
                sheet::UNINSTALL,
                sheet::APP_INFO,
                sheet::CLOSE
            ]
        );
        assert_eq!(ids(&report.apps[2]), [sheet::APP_INFO, sheet::CLOSE]);
        let facts = app_sheet(&report.apps[1], &report, abilities).facts;
        assert!(
            facts
                .iter()
                .any(|f| f.starts_with("Last opened 13 months ago"))
        );
        assert!(facts.iter().any(|f| f.starts_with("No app store has it")));
    }

    fn junk(kind: JunkKind, bytes: u64, names: &[&str]) -> Junk {
        Junk {
            kind,
            bytes,
            paths: names.iter().map(|n| format!("/x/{n}").into()).collect(),
            names: names.iter().map(|n| n.to_string()).collect(),
        }
    }

    #[test]
    fn free_up_space_lists_junk_worth_showing() {
        const MB: u64 = 1024 * 1024;
        let mut report = report();
        report.junk = vec![
            junk(
                JunkKind::Trash,
                5 * MB,
                &["a.jpg", "b.jpg", "c.jpg", "d.jpg"],
            ),
            junk(JunkKind::Thumbnails, 1000, &["t"]),
            junk(JunkKind::Leftovers, 2 * MB, &["Gone"]),
        ];
        let home = rows(&AppsState::Ready(report.clone()), options(View::All));
        let keys: Vec<_> = of_kind(&home, NOTICE)
            .iter()
            .map(|r| r.key.to_string())
            .collect();
        assert_eq!(keys, ["clean:trash", "clean:leftovers"]);
        let trash = home.iter().find(|r| r.key == "clean:trash").unwrap();
        assert_eq!(trash.title, "Empty the trash: 5.00 MB");
        assert!(trash.subtitle.contains("a.jpg, b.jpg, c.jpg and 1 more"));
        assert_eq!(
            junk_confirm(&report.junk[2]),
            "Permanently delete 1 folder left by removed apps (2.00 MB)? Their files can't \
             be restored."
        );
        let mut no_access = options(View::All);
        no_access.storage_access = false;
        let limited = rows(&AppsState::Ready(report), no_access);
        let keys: Vec<_> = of_kind(&limited, NOTICE)
            .iter()
            .map(|r| r.key.to_string())
            .collect();
        assert_eq!(keys, [GRANT_STORAGE]);
    }

    #[test]
    fn media_view_and_sheet() {
        const MB: u64 = 1024 * 1024;
        let mut report = report();
        let root = locations::default_path();
        report.media = vec![
            MediaUse {
                package: "big.app".into(),
                bytes: 300 * MB,
                files: 12,
                folder: root.join("Pictures/Big"),
                in_app_data: false,
            },
            MediaUse {
                package: "not.installed".into(),
                bytes: 900 * MB,
                files: 1,
                folder: root.join("Movies"),
                in_app_data: false,
            },
        ];
        let rows = rows(&AppsState::Ready(report.clone()), options(View::Media));
        let group = rows.iter().find(|r| r.kind == GROUP && r.selected).unwrap();
        assert_eq!(group.detail, "1 app · 300 MB");
        let apps = of_kind(&rows, APP);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].subtitle, "12 files, mostly in Pictures/Big");
        assert_eq!(apps[0].detail, "300 MB");
        assert!(apps[0].tags.ends_with("300 MB of files"));

        let sheet = app_sheet(&report.apps[0], &report, options(View::All).abilities);
        assert!(
            sheet
                .facts
                .iter()
                .any(|f| f == "Saved 300 MB to your storage (12 files), mostly in Pictures/Big.")
        );
        assert!(sheet.actions.iter().any(|a| a.0 == sheet::BROWSE));
    }

    #[test]
    fn biggest_caches_and_what_clearing_freed() {
        const MB: u64 = 1024 * 1024;
        let mut report = report();
        report.apps[0].cache_bytes = 300 * MB;
        report.apps[0].data_bytes = 900 * MB;
        report.apps[1].cache_bytes = 40 * MB;
        report.apps[1].data_bytes = 60 * MB;
        let home = rows(&AppsState::Ready(report.clone()), options(View::Cache));
        let group = home.iter().find(|r| r.kind == GROUP && r.selected).unwrap();
        assert_eq!(group.detail, "2 apps · 340 MB");
        let apps = of_kind(&home, APP);
        assert_eq!(apps[0].detail, "300 MB");
        assert_eq!(apps[1].detail, "40.0 MB");

        let mut cleared = options(View::All);
        cleared.cache_freed = Some(2 * MB);
        let home = rows(&AppsState::Ready(report), cleared);
        let card = home.iter().find(|r| r.key == CLEAR_CACHE).unwrap();
        assert!(
            card.subtitle
                .starts_with("Android's cache cleaner only freed 2.00 MB")
        );
    }

    #[test]
    fn growing_apps_and_storage_chart() {
        use crate::growth::Change;
        use crate::history::StorageSample;
        const MB: u64 = 1024 * 1024;
        let mut report = report();
        report.apps[0].growth = Some(Change {
            bytes: 300 * MB as i64,
            days: 30,
        });
        report.apps[1].growth = Some(Change {
            bytes: 20 * MB as i64,
            days: 30,
        });
        let home = rows(&AppsState::Ready(report.clone()), options(View::Growing));
        let group = home.iter().find(|r| r.kind == GROUP && r.selected).unwrap();
        assert_eq!(group.detail, "1 app · +300 MB");
        let apps = of_kind(&home, APP);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].detail, "+300 MB");
        assert!(apps[0].tags.contains("+300 MB in 30 days"));
        let sheet = app_sheet(&report.apps[0], &report, Abilities::default());
        assert!(
            sheet
                .facts
                .contains(&"Grew 300 MB over the last 30 days.".to_string())
        );
        assert!(of_kind(&home, CHART).is_empty());

        let sample = |day: u64| StorageSample {
            day,
            total: 1000 * MB,
            free: 500 * MB,
            apps: 200 * MB,
            files: 100 * MB,
        };
        report.storage = vec![sample(20_000), sample(20_010)];
        let home = rows(&AppsState::Ready(report), options(View::All));
        let chart = of_kind(&home, CHART);
        assert_eq!(chart.len(), 1);
        assert_eq!(chart[0].subtitle, "Used space hardly changed in 10 days.");
        assert!(chart[0].chart_used.starts_with("M 0 100 L 0.0 50.0"));
    }

    #[test]
    fn removed_sheet_links_to_the_store() {
        let sheet = removed_sheet(&report().removed[0], NOW);
        assert_eq!(sheet.actions[0].0, sheet::STORE);
        assert_eq!(sheet.actions[0].1, "Open in Play Store");
        assert!(sheet.facts[0].starts_with("Removed 10 days ago"));
    }
}
