//! Rows of the app's places other than a scan: Home (drives and folders to
//! scan and, where the platform reports it, how the device's storage
//! divides up between apps, the user's files and the system), Clean, Apps,
//! Quarantine, Hub and Settings. The controller adds the rows other modules
//! make (junk, the quarantine, updates).

use freesize_core::{DiskSpace, disk_space, format_count, format_size};

use std::path::Path;

use crate::apps::{Abilities, AppReport, AppUsage};
use crate::cleanup::{Junk, JunkKind, MediaUse, Review, ReviewKind};
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
/// Prefix of the action ids that open a group of apps as its own page.
pub const VIEW_PREFIX: &str = "view:";
/// Closes the open group's page (its back arrow; `app.slint` sends it too).
pub const BACK: &str = "back";
/// Prefix of the keys of removed apps' rows; installed apps use the bare
/// package name.
pub const REMOVED_PREFIX: &str = "removed:";
/// Prefix of the action ids that delete a kind of junk (see `JunkKind::id`).
pub const CLEAN_PREFIX: &str = "clean:";
/// Prefix of the action ids that open files to review (see `ReviewKind::id`).
pub const REVIEW_PREFIX: &str = "review:";
/// The FreeSize Hub card's buttons.
pub const HUB_CONNECT: &str = "hub-connect";
pub const HUB_SETTINGS: &str = "hub-settings";
pub const HUB_REVIEW: &str = "hub-review";
/// Switches the app group between a list and bubbles.
pub const TOGGLE_BUBBLES: &str = "toggle-bubbles";
/// Leaves the app the bubbles zoomed into.
pub const ZOOM_OUT: &str = "zoom-out";

/// Junk and app media smaller than this isn't worth a row.
const WORTH_SHOWING: u64 = 1024 * 1024;
/// Files to review are suggested from this size on.
const WORTH_REVIEWING: u64 = 50 * 1024 * 1024;
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
    pub const HUB_REVOKE: &str = "hub-revoke";
    pub const HUB_DISCONNECT: &str = "hub-disconnect";
}

/// The places of the app, in the sidebar on wide windows and the tab bar on
/// phones.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Home,
    Clean,
    /// Android's app groups.
    Apps,
    /// Its own place on desktops; on phones it's part of Clean.
    Quarantine,
    /// Its own place on desktops; on phones it's part of Settings.
    Hub,
    Settings,
}

impl Section {
    /// The places this platform has, in order.
    pub fn all() -> &'static [Section] {
        if cfg!(target_os = "android") {
            &[
                Section::Home,
                Section::Clean,
                Section::Apps,
                Section::Settings,
            ]
        } else {
            &[
                Section::Home,
                Section::Clean,
                Section::Quarantine,
                Section::Hub,
                Section::Settings,
            ]
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Section::Home => "home",
            Section::Clean => "clean",
            Section::Apps => "apps",
            Section::Quarantine => "quarantine",
            Section::Hub => "hub",
            Section::Settings => "settings",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Section::Home => "Home",
            Section::Clean => "Clean",
            Section::Apps => "Apps",
            Section::Quarantine => "Quarantine",
            Section::Hub => "Hub",
            Section::Settings => "Settings",
        }
    }

    pub fn from_key(key: &str) -> Section {
        Section::all()
            .iter()
            .copied()
            .find(|s| s.key() == key)
            .unwrap_or_default()
    }

    /// Where the Hub's rows go: their own place, or Settings on phones.
    pub fn has_hub(self) -> bool {
        match self {
            Section::Hub => true,
            Section::Settings => !Section::all().contains(&Section::Hub),
            _ => false,
        }
    }

    /// Where the quarantine's rows go: their own place, or Clean on phones.
    pub fn has_quarantine(self) -> bool {
        match self {
            Section::Quarantine => true,
            Section::Clean => !Section::all().contains(&Section::Quarantine),
            _ => false,
        }
    }
}

const DRIVE: i32 = 0;
const HEADING: i32 = 1;
const BREAKDOWN: i32 = 2;
const APP: i32 = 3;
const NOTICE: i32 = 4;
const GROUP: i32 = 5;
const CHART: i32 = 6;
const BUBBLES: i32 = 7;
const PAGE: i32 = 8;
const CHOICE: i32 = 9;
const TEXT: i32 = 10;

/// Android keeps roughly this much usage history.
const USAGE_HISTORY: u64 = 2 * 365 * 24 * 60 * 60;

/// What is known about app storage. Only one exists, so the size of the
/// report doesn't matter.
#[allow(clippy::large_enum_variant)]
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
    /// The place shown.
    pub section: Section,
    /// The group of apps opened as its own page; none shows the home screen.
    pub page: Option<View>,
    /// A new measurement is running; the previous one stays on screen.
    pub refreshing: bool,
    pub storage_access: bool,
    pub abilities: Abilities,
    /// What Android's clear-cache dialog freed the last time it was used.
    pub cache_freed: Option<u64>,
    /// Past sizes of the drives, for their trends.
    pub drives: Option<&'a DriveLog>,
    /// Show the app group as bubbles, zoomed into this app if set.
    pub bubbles: bool,
    pub zoom: Option<&'a str>,
    /// The FreeSize Hub card; none leaves the section out.
    pub hub: Option<&'a HubCard>,
}

/// What the FreeSize Hub card shows.
#[derive(Clone, Debug, Default)]
pub struct HubCard {
    /// The Hub's name when connected.
    pub hub: Option<String>,
    pub status: String,
    /// Requests set aside with "Not now".
    pub waiting: usize,
}

pub fn rows(apps: &AppsState, options: Options<'_>) -> Vec<HomeRow> {
    if let (Some(view), AppsState::Ready(report)) = (options.page, apps) {
        return page_rows(report, view, options);
    }
    let mut rows = Vec::new();
    match options.section {
        Section::Home => {
            rows.push(heading("Choose a drive or folder to analyze", "", None));
            rows.extend(
                locations::list()
                    .iter()
                    .map(|location| drive_row(location, options.drives)),
            );
            // Requests waiting are worth seeing first thing.
            if let Some(card) = options.hub.filter(|c| c.hub.is_some() && c.waiting > 0) {
                rows.push(waiting_row(card));
            }
            let title = "Apps & system";
            if let Some(state) = apps_state_rows(apps, title) {
                rows.extend(state);
            } else if let AppsState::Ready(report) = apps {
                if options.refreshing {
                    rows.push(heading(title, "Updating…", None));
                } else {
                    rows.push(heading(title, "", Some(("Refresh", REFRESH_APPS))));
                }
                if let Some(space) = disk_space(&locations::default_path()) {
                    rows.push(breakdown_row(report, space));
                }
                rows.extend(chart_row(report));
            }
        }
        Section::Clean => {
            if let AppsState::Ready(report) = apps {
                let cards = free_up_rows(report, options);
                if cards.is_empty() {
                    rows.push(heading("Free up space", "", None));
                    rows.push(notice(
                        "Nothing worth freeing right now",
                        "No big caches, old installers, trash or leftovers turned up. Scan \
                         your storage on Home to see what takes the space.",
                        None,
                    ));
                }
                rows.extend(cards);
            } else if let Some(state) = apps_state_rows(apps, "Free up space") {
                rows.extend(state);
            }
        }
        Section::Apps => {
            if let Some(state) = apps_state_rows(apps, "Your apps") {
                rows.extend(state);
            } else if let AppsState::Ready(report) = apps {
                if options.refreshing {
                    rows.push(heading("Your apps", "Updating…", None));
                } else {
                    rows.push(heading("Your apps", "", Some(("Refresh", REFRESH_APPS))));
                }
                rows.extend(
                    View::ALL
                        .into_iter()
                        .filter_map(|view| group_row(report, view)),
                );
            }
        }
        Section::Quarantine | Section::Hub | Section::Settings => {}
    }
    if options.section.has_hub()
        && let Some(card) = options.hub
    {
        rows.extend(hub_rows(card));
    }
    rows
}

/// What stands in for the apps while they can't be shown: the way to
/// Usage access, a measurement running, or what went wrong. None once
/// they're measured, or where the platform can't.
fn apps_state_rows(apps: &AppsState, title: &str) -> Option<Vec<HomeRow>> {
    let mut rows = vec![heading(title, "", None)];
    match apps {
        AppsState::Unsupported | AppsState::Ready(_) => return None,
        AppsState::NeedsAccess { asked } => {
            rows.push(notice(
                "See what apps and Android are using",
                "Most of a phone's storage is app data and the system, which Android hides \
                 from file scans. Allow Usage access and FreeSize can show how much space \
                 each app takes.",
                Some(("Allow usage access", GRANT_USAGE_ACCESS)),
            ));
            if *asked {
                rows.push(notice(
                    "Switch greyed out, or “App was denied access”?",
                    "Android blocks Usage access for apps installed from a downloaded file \
                     until you allow it. Open FreeSize's app info, tap ⋮ in the top corner, \
                     choose “Allow restricted settings” and confirm. Then tap Allow usage \
                     access again.",
                    Some(("Open FreeSize's app info", OPEN_APP_INFO)),
                ));
            }
        }
        AppsState::Loading => rows.push(notice("Measuring apps…", "", None)),
        AppsState::Failed(err) => rows.push(notice(
            "Couldn't measure apps",
            err,
            Some(("Try again", REFRESH_APPS)),
        )),
    }
    Some(rows)
}

/// A setting with a few choices, shown as a drop-down.
pub(crate) fn choice(
    title: &str,
    text: &str,
    key: &str,
    choices: &[&str],
    chosen: usize,
) -> HomeRow {
    let choices: Vec<slint::SharedString> = choices.iter().map(|c| (*c).into()).collect();
    HomeRow {
        kind: CHOICE,
        title: title.into(),
        subtitle: text.into(),
        key: key.into(),
        choices: slint::ModelRc::new(slint::VecModel::from(choices)),
        chosen: chosen as i32,
        ..Default::default()
    }
}

/// A setting typed in a box, saved with Enter or its Save button.
pub(crate) fn text_setting(title: &str, text: &str, key: &str, value: &str) -> HomeRow {
    HomeRow {
        kind: TEXT,
        title: title.into(),
        subtitle: text.into(),
        key: key.into(),
        detail: value.into(),
        ..Default::default()
    }
}

/// A notice with a second, plain button beside its first.
pub(crate) fn notice2(
    title: &str,
    text: &str,
    action: (&str, &str),
    second: (&str, &str),
) -> HomeRow {
    HomeRow {
        action2: second.0.into(),
        key2: second.1.into(),
        ..notice(title, text, Some(action))
    }
}

/// A group of apps on its own page: a header with the way back, then the
/// apps, as a list or as bubbles.
fn page_rows(report: &AppReport, view: View, options: Options<'_>) -> Vec<HomeRow> {
    let toggle = if view == View::Removed {
        ""
    } else if options.bubbles {
        "List"
    } else {
        "Bubbles"
    };
    let (count, bytes) = group_size(report, view);
    let mut rows = vec![HomeRow {
        kind: PAGE,
        title: view.title().into(),
        detail: group_detail(view, count, bytes).into(),
        subtitle: explanation(view, options).into(),
        action: toggle.into(),
        key: TOGGLE_BUBBLES.into(),
        ..Default::default()
    }];
    let before = rows.len();
    if options.bubbles && view != View::Removed {
        // The bubbles themselves come from `appbubbles`; this row
        // holds them, with the app zoomed into above them.
        let zoomed = options
            .zoom
            .and_then(|p| report.apps.iter().find(|a| a.package == p));
        rows.push(HomeRow {
            kind: BUBBLES,
            title: zoomed
                .map(|app| {
                    let name = if app.label.is_empty() {
                        &app.package
                    } else {
                        &app.label
                    };
                    format!("{name} · {}", format_size(app.size()))
                })
                .unwrap_or_default()
                .into(),
            subtitle: if zoomed.is_some() {
                "Tap a part for the app's details; Saved files opens them.".into()
            } else {
                "Tap an app to see what its space is made of.".into()
            },
            action: if zoomed.is_some() { "Back" } else { "" }.into(),
            key: ZOOM_OUT.into(),
            ..Default::default()
        });
    } else if view == View::Removed {
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

pub(crate) fn heading(title: &str, text: &str, action: Option<(&str, &str)>) -> HomeRow {
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

pub(crate) fn notice(title: &str, text: &str, action: Option<(&str, &str)>) -> HomeRow {
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

/// The cache Android's cache cleaner can reach: only the folders in shared
/// storage (`Android/data/<app>/cache`). Where Android doesn't say how much
/// that is (before 12), all of it, as the most it could be.
pub fn clearable_cache(report: &AppReport, abilities: Abilities) -> u64 {
    report
        .apps
        .iter()
        .map(|a| {
            if abilities.shared_cache_known {
                a.external_cache_bytes
            } else {
                a.cache_bytes
            }
        })
        .sum()
}

/// App caches, and honestly what can clear them: Android doesn't let one app
/// clear another's private cache, so its cache cleaner only empties the
/// part in shared storage. The rest goes when Android needs the space, or
/// app by app from their info pages.
fn cache_card(report: &AppReport, options: &Options<'_>, cache: u64) -> HomeRow {
    const WORTH_CLEARING: u64 = 50 * 1024 * 1024;
    let abilities = options.abilities;
    let shared = abilities
        .shared_cache_known
        .then(|| clearable_cache(report, abilities));
    let can_clear = abilities.clear_cache && shared.is_none_or(|bytes| bytes >= WORTH_CLEARING);
    let mut text = String::new();
    if let Some(freed) = options.cache_freed {
        text.push_str(&format!(
            "Android's cache cleaner freed {} last time. ",
            format_size(freed)
        ));
    }
    text.push_str(
        "Android clears caches by itself when space runs low, and doesn't let other apps \
         clear them. ",
    );
    text.push_str(&match (shared, can_clear) {
        (Some(bytes), true) => format!(
            "Its cache cleaner can only empty the {} in shared storage; for the rest, \
             clear the apps under Biggest caches one by one.",
            format_size(bytes)
        ),
        (Some(bytes), false) => format!(
            "Only {} is in shared storage, where its cache cleaner works; to free more \
             now, clear the apps under Biggest caches one by one.",
            format_size(bytes)
        ),
        (None, true) => "Its cache cleaner only empties the part in shared storage, usually \
                         small; for the rest, clear the apps under Biggest caches one by one."
            .into(),
        (None, false) => "To free it now, clear the apps under Biggest caches one by one.".into(),
    });
    let clear_label = match shared {
        Some(bytes) => format!("Clear {}", format_size(bytes)),
        None => "Clear shared cache".into(),
    };
    let biggest = format!("{VIEW_PREFIX}{}", View::Cache.id());
    let action = if can_clear {
        (clear_label.as_str(), CLEAR_CACHE)
    } else {
        ("Biggest caches", biggest.as_str())
    };
    notice(
        &format!("App caches: {}", format_size(cache)),
        &text,
        Some(action),
    )
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
                "FreeSize needs All files access to look through your files for space you \
                 can free, and to clear every app's cache.",
                Some(("Grant access", GRANT_STORAGE)),
            ),
        ];
    }
    let mut cards = Vec::new();
    let cache: u64 = report.apps.iter().map(|a| a.cache_bytes).sum();
    if cache >= CACHE_WORTH_IT {
        cards.push(cache_card(report, &options, cache));
    }
    // The trash, installers and the rest: one card, opening the Clean junk
    // screen where each kind is ticked or not.
    let worth: Vec<&Junk> = JunkKind::ALL
        .iter()
        .filter_map(|&kind| {
            report.junk.iter().find(|j| {
                j.kind == kind
                    && (j.bytes >= WORTH_SHOWING || (kind.counted() && !j.paths.is_empty()))
            })
        })
        .collect();
    if !worth.is_empty() {
        cards.push(junk_card(&worth));
    }
    for kind in ReviewKind::ALL {
        if let Some(review) = report
            .reviews
            .iter()
            .find(|r| r.kind == kind && r.bytes >= WORTH_REVIEWING)
        {
            cards.push(review_row(review));
        }
    }
    if cards.is_empty() {
        return cards;
    }
    let mut rows = vec![heading(title, "", None)];
    rows.extend(cards);
    rows
}

/// The FreeSize Hub card: connect, or how the connection is going.
fn hub_rows(card: &HubCard) -> Vec<HomeRow> {
    let mut rows = vec![heading("FreeSize Hub", "", None)];
    match &card.hub {
        None => rows.push(notice(
            "Connect to a FreeSize Hub",
            "Show this device's storage in your Hub's dashboard, next to your other devices. \
             The Hub can ask this device to free space; nothing happens unless you approve it \
             here.",
            Some(("Connect", HUB_CONNECT)),
        )),
        Some(name) => {
            if card.waiting > 0 {
                rows.push(waiting_row(card));
            }
            let status = if card.status.is_empty() {
                format!("Connected to {name}")
            } else {
                card.status.clone()
            };
            rows.push(notice(
                &status,
                "Sends a summary after each scan. Anything the Hub asks for is shown here to \
                 approve or deny first.",
                Some(("Hub settings", HUB_SETTINGS)),
            ));
        }
    }
    rows
}

/// Requests set aside with Not now.
fn waiting_row(card: &HubCard) -> HomeRow {
    notice(
        &count(
            card.waiting,
            "request waiting for you",
            "requests waiting for you",
        ),
        "Set aside with Not now. The Hub withdraws requests after a day.",
        Some(("Review", HUB_REVIEW)),
    )
}

/// The Hub settings sheet.
pub fn hub_sheet(state: &freesize_agent::AgentState, status: &str, now: i64) -> Sheet {
    let fingerprint = state.fingerprint();
    let mut facts = vec![
        format!("This device's key: {fingerprint}"),
        format!(
            "A server or NAS can make this device its approver: on the server, run \
             freesize approver set {fingerprint}. Its requests then come here to approve."
        ),
    ];
    // Good news is a plain line; only trouble goes in the highlighted note.
    let fine = status.starts_with("Connected");
    if fine {
        facts.insert(0, format!("{status}."));
    }
    let grants: Vec<_> = state.grants.iter().filter(|g| g.expires > now).collect();
    if grants.is_empty() {
        facts.push("Every request from the Hub asks you first.".into());
    }
    for grant in &grants {
        facts.push(format!(
            "{} may {} without asking for {} more minutes.",
            grant.admin_name,
            grant.kind.label(),
            ((grant.expires - now) / 60).max(1)
        ));
    }
    let mut actions = Vec::new();
    if !grants.is_empty() {
        actions.push((sheet::HUB_REVOKE, "End grants".to_string(), false));
    }
    actions.push((sheet::HUB_DISCONNECT, "Disconnect".to_string(), false));
    actions.push((sheet::CLOSE, "Close".to_string(), true));
    Sheet {
        title: state.hub_name.clone(),
        subtitle: format!("{} · Hub key {}", state.hub, state.hub_key.fingerprint()),
        facts,
        note: if fine {
            String::new()
        } else {
            status.to_string()
        },
        actions,
    }
}

pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    format!(
        "{} {}",
        format_count(n as u64),
        if n == 1 { one } else { many }
    )
}

/// "Pictures/Screenshots": a folder and the one it's in.
fn short_folder(path: &Path) -> String {
    let parts: Vec<_> = path.iter().collect();
    parts[parts.len().saturating_sub(2)..]
        .iter()
        .collect::<std::path::PathBuf>()
        .display()
        .to_string()
}

/// The phone's Clean junk card: what each kind holds, and the way to the
/// screen that cleans them.
fn junk_card(found: &[&Junk]) -> HomeRow {
    let total: u64 = found.iter().map(|j| j.bytes).sum();
    let kinds: Vec<String> = found
        .iter()
        .map(|j| {
            let name = match j.kind {
                JunkKind::Trash => "the trash",
                JunkKind::Installers => "old installers",
                JunkKind::Thumbnails => "thumbnails",
                JunkKind::Leftovers => "leftovers of removed apps",
                JunkKind::OldDownloads => "old downloads",
                JunkKind::ScreenRecordings => "screen recordings",
                JunkKind::EmptyFolders => "empty folders",
            };
            if j.bytes > 0 {
                format!("{name} ({})", format_size(j.bytes))
            } else {
                name.to_string()
            }
        })
        .collect();
    let mut list = kinds.join(", ");
    if let Some(first) = list.get(..1) {
        list = first.to_uppercase() + &list[1..];
    }
    notice(
        &format!("Clean junk: {}", format_size(total)),
        &format!(
            "{list}. Tick what goes on the next screen; nothing is deleted until you \
             confirm."
        ),
        Some(("Review", crate::junk::OPEN)),
    )
}

fn review_row(review: &Review) -> HomeRow {
    let size = format_size(review.bytes);
    let (title, text) = match review.kind {
        ReviewKind::Screenshots => {
            let folders: Vec<String> = review
                .places
                .iter()
                .map(|place| short_folder(&place.folder))
                .collect();
            (
                format!("Look through old screenshots: {size}"),
                format!(
                    "{} older than a month in {}. Screenshots are often needed only for a \
                     moment: open them, collect the ones you're done with and delete them \
                     together.",
                    count(review.files as usize, "screenshot", "screenshots"),
                    folders.join(" and ")
                ),
            )
        }
        ReviewKind::ChatMedia => {
            // Apps with more than one folder count once.
            let mut apps: Vec<(String, u64)> = Vec::new();
            for place in &review.places {
                match apps.iter_mut().find(|(name, _)| *name == place.name) {
                    Some((_, bytes)) => *bytes += place.bytes,
                    None => apps.push((place.name.clone(), place.bytes)),
                }
            }
            apps.sort_by_key(|(_, bytes)| std::cmp::Reverse(*bytes));
            let named: Vec<String> = apps
                .iter()
                .map(|(name, bytes)| format!("{name} ({})", format_size(*bytes)))
                .collect();
            let list = match named.as_slice() {
                [] => String::new(),
                [one] => one.clone(),
                [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
            };
            (
                format!("Look through chat media: {size}"),
                format!(
                    "Photos, videos and voice notes saved by {list}. Chats keep everything \
                     anyone ever sent you; Review lists the biggest files first."
                ),
            )
        }
    };
    let key = format!("{REVIEW_PREFIX}{}", review.kind.id());
    notice(&title, &text, Some(("Review", &key)))
}

/// The sheet listing the folders of a review with more than one.
pub fn review_sheet(review: &Review, ids: &[&'static str]) -> Sheet {
    let what = match review.kind {
        ReviewKind::Screenshots => "Old screenshots",
        ReviewKind::ChatMedia => "Chat media",
    };
    let mut actions: Vec<(&'static str, String, bool)> = review
        .places
        .iter()
        .zip(ids)
        .map(|(place, &id)| {
            let label = format!(
                "{} · {} · {}",
                place.name,
                short_folder(&place.folder),
                format_size(place.bytes)
            );
            (id, label, false)
        })
        .collect();
    actions.push((sheet::CLOSE, "Close".into(), false));
    Sheet {
        title: what.into(),
        subtitle: format!(
            "{} in {}",
            format_size(review.bytes),
            count(review.files as usize, "file", "files")
        ),
        facts: vec![
            "Pick a folder to see its files. Open any of them to look at it, collect the ones \
             you don't need, then delete or move them together from the basket."
                .into(),
        ],
        note: String::new(),
        actions,
    }
}

/// The question asked before deleting a kind of junk.
pub fn junk_confirm(junk: &Junk) -> String {
    let size = format_size(junk.bytes);
    let n = junk.paths.len();
    match junk.kind {
        JunkKind::Trash => format!(
            "Move {} from the trash to the quarantine ({size})? Put back returns them; the \
             space is freed when you empty the quarantine.",
            count(n, "item", "items")
        ),
        JunkKind::Installers => format!(
            "Move {} from Download to the quarantine ({size})?",
            count(n, "APK file", "APK files")
        ),
        JunkKind::Thumbnails => {
            format!(
                "Move the thumbnail cache ({size}) to the quarantine? Gallery apps rebuild it as \
                 needed."
            )
        }
        JunkKind::Leftovers => format!(
            "Move {} left by removed apps ({size}) to the quarantine? Put back returns them if \
             an app turns out to need them.",
            count(n, "folder", "folders")
        ),
        JunkKind::OldDownloads => format!(
            "Move {} from Download to the quarantine ({size})? Put back returns them; the \
             space is freed when you empty the quarantine.",
            count(n, "old file", "old files")
        ),
        JunkKind::ScreenRecordings => format!(
            "Move {} to the quarantine ({size})? Put back returns them.",
            count(n, "screen recording", "screen recordings")
        ),
        JunkKind::EmptyFolders => format!(
            "Move {} to the quarantine? They hold nothing; Put back returns them.",
            count(n, "empty folder", "empty folders")
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

/// How many apps are in a group and how much they hold (or grew, or saved).
fn group_size(report: &AppReport, view: View) -> (usize, u64) {
    if view == View::Removed {
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
    }
}

/// "40 apps · 4.82 GB"
fn group_detail(view: View, count: usize, bytes: u64) -> String {
    let apps = if count == 1 { "app" } else { "apps" };
    if bytes > 0 && view == View::Growing {
        format!("{count} {apps} · +{}", format_size(bytes))
    } else if bytes > 0 {
        format!("{count} {apps} · {}", format_size(bytes))
    } else {
        format!("{count} {apps}")
    }
}

/// A row that opens `view` as a page. Groups with nothing in them are left
/// out.
fn group_row(report: &AppReport, view: View) -> Option<HomeRow> {
    let (count, bytes) = group_size(report, view);
    if count == 0 && view != View::All {
        return None;
    }
    Some(HomeRow {
        kind: GROUP,
        title: view.title().into(),
        detail: group_detail(view, count, bytes).into(),
        key: format!("{VIEW_PREFIX}{}", view.id()).into(),
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
                          piling up. FreeSize measures apps each day it's opened."
            .into(),
        View::Removed => "Apps FreeSize has seen that are no longer installed. Tap one to \
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
        _ if app.own => "This is FreeSize.".into(),
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
            "Removed {}: FreeSize noticed it was gone on {}.",
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
             Download/FreeSize if you saved it there."
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

    /// The home screen.
    fn options() -> Options<'static> {
        Options {
            storage_access: true,
            abilities: Abilities {
                uninstall: true,
                archive: true,
                clear_cache: true,
                shared_cache_known: true,
                save_apk: true,
            },
            ..Options::default()
        }
    }

    /// The Clean section.
    fn clean() -> Options<'static> {
        Options {
            section: Section::Clean,
            ..options()
        }
    }

    /// The page of a group of apps.
    fn page(view: View) -> Options<'static> {
        Options {
            page: Some(view),
            ..options()
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
        let rows = rows(&AppsState::Ready(report()), page(View::All));
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
    fn groups_show_counts_and_open_as_pages() {
        let home = rows(
            &AppsState::Ready(report()),
            Options {
                section: Section::Apps,
                ..options()
            },
        );
        let groups: Vec<_> = of_kind(&home, GROUP)
            .iter()
            .map(|r| (r.title.to_string(), r.detail.to_string(), r.key.to_string()))
            .collect();
        assert_eq!(
            groups,
            [
                (
                    "All apps".into(),
                    "2 apps · 1.01 KB".into(),
                    "view:all".into()
                ),
                (
                    "Not opened in 3 months".into(),
                    "1 app · 30 B".into(),
                    "view:unused".into()
                ),
                (
                    "Not from an app store".into(),
                    "1 app · 30 B".into(),
                    "view:not-from-store".into()
                ),
                ("Removed apps".into(), "1 app".into(), "view:removed".into()),
            ]
        );
        // The apps are on the groups' pages, not under the groups.
        assert!(of_kind(&home, APP).is_empty());

        let unused = rows(&AppsState::Ready(report()), page(View::Unused));
        assert_eq!(unused[0].kind, PAGE);
        assert_eq!(unused[0].title, "Not opened in 3 months");
        assert_eq!(unused[0].detail, "1 app · 30 B");
        assert_eq!(unused[0].action, "Bubbles");
        assert!(of_kind(&unused, GROUP).is_empty());
        assert!(of_kind(&unused, DRIVE).is_empty());
        let apps = of_kind(&unused, APP);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].key, "tool.app");
    }

    #[test]
    fn a_page_needs_the_apps_measured() {
        let rows = rows(&AppsState::Loading, page(View::Cache));
        assert!(of_kind(&rows, PAGE).is_empty());
        assert_eq!(of_kind(&rows, NOTICE)[0].title, "Measuring apps…");
    }

    #[test]
    fn removed_view_lists_history() {
        let rows = rows(&AppsState::Ready(report()), page(View::Removed));
        // Removed apps have no bubbles.
        assert_eq!(rows[0].action, "");
        let apps = of_kind(&rows, APP);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].key, "removed:gone.app");
        assert_eq!(apps[0].subtitle, "Removed 10 days ago · was 2.00 KB");
    }

    #[test]
    fn clear_cache_only_offers_what_android_can_clear() {
        const MB: u64 = 1024 * 1024;
        let mut report = report();
        let rows_now = rows(&AppsState::Ready(report.clone()), clean());
        assert!(!rows_now.iter().any(|r| r.title.starts_with("App caches")));
        report.apps[0].cache_bytes = 100 * MB;
        report.apps[0].data_bytes = 200 * MB;
        // All of it private: Android's cleaner can't reach it, so the card
        // points to clearing app by app.
        let rows_now = rows(&AppsState::Ready(report.clone()), clean());
        let card = rows_now
            .iter()
            .find(|r| r.title == "App caches: 100 MB")
            .unwrap();
        assert_eq!(card.key, "view:cache");
        assert!(card.subtitle.contains("Only 0 B is in shared storage"));
        // Enough in shared storage: the cleaner, for that much.
        report.apps[0].external_cache_bytes = 60 * MB;
        let rows_now = rows(&AppsState::Ready(report.clone()), clean());
        let card = rows_now.iter().find(|r| r.key == CLEAR_CACHE).unwrap();
        assert_eq!(card.action, "Clear 60.0 MB");
        assert_eq!(clearable_cache(&report, options().abilities), 60 * MB);
        // Before Android 12 the split is unknown: offered, with the caveat.
        let mut old = clean();
        old.abilities.shared_cache_known = false;
        let rows_now = rows(&AppsState::Ready(report.clone()), old);
        let card = rows_now.iter().find(|r| r.key == CLEAR_CACHE).unwrap();
        assert_eq!(card.action, "Clear shared cache");
        assert!(card.subtitle.contains("usually small"));
        let mut no_access = clean();
        no_access.storage_access = false;
        let rows_now = rows(&AppsState::Ready(report), no_access);
        assert!(rows_now.iter().any(|r| r.key == GRANT_STORAGE));
    }

    #[test]
    fn sheet_offers_saving_apps_no_store_has() {
        let report = report();
        let abilities = options().abilities;
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
    fn review_cards_and_sheet() {
        use crate::cleanup::Place;
        const MB: u64 = 1024 * 1024;
        let place = |name: &str, folder: &str, mb: u64| Place {
            name: name.into(),
            folder: folder.into(),
            bytes: mb * MB,
            files: 1500,
        };
        let mut report = report();
        report.reviews = vec![
            Review {
                kind: ReviewKind::Screenshots,
                bytes: 20 * MB,
                files: 30,
                places: vec![place("Screenshots", "/s/Pictures/Screenshots", 20)],
            },
            Review {
                kind: ReviewKind::ChatMedia,
                bytes: 600 * MB,
                files: 4500,
                places: vec![
                    place("Telegram", "/s/Movies/Telegram", 300),
                    place("WhatsApp", "/s/WhatsApp/Media", 200),
                    place("Telegram", "/s/Pictures/Telegram", 100),
                ],
            },
        ];
        let home = rows(&AppsState::Ready(report.clone()), clean());
        // Too few old screenshots to suggest.
        assert!(home.iter().all(|r| r.key != "review:screenshots"));
        let chat = home.iter().find(|r| r.key == "review:chat-media").unwrap();
        assert_eq!(chat.title, "Look through chat media: 600 MB");
        assert!(
            chat.subtitle
                .contains("saved by Telegram (400 MB) and WhatsApp (200 MB).")
        );
        assert_eq!(chat.action, "Review");

        let sheet = review_sheet(&report.reviews[1], &["a", "b", "c", "d"]);
        assert_eq!(sheet.subtitle, "600 MB in 4,500 files");
        // Windows shows its own separator.
        let labels: Vec<String> = sheet
            .actions
            .iter()
            .map(|a| a.1.replace('\\', "/"))
            .collect();
        assert_eq!(
            labels,
            [
                "Telegram · Movies/Telegram · 300 MB",
                "WhatsApp · WhatsApp/Media · 200 MB",
                "Telegram · Pictures/Telegram · 100 MB",
                "Close"
            ]
        );
        assert_eq!(sheet.actions[0].0, "a");
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
        let home = rows(&AppsState::Ready(report.clone()), clean());
        let keys: Vec<_> = of_kind(&home, NOTICE)
            .iter()
            .map(|r| r.key.to_string())
            .collect();
        // One card for all of it; the thumbnails are too small to name.
        assert_eq!(keys, [crate::junk::OPEN]);
        let card = home.iter().find(|r| r.key == crate::junk::OPEN).unwrap();
        assert_eq!(card.title, "Clean junk: 7.00 MB");
        assert!(
            card.subtitle
                .starts_with("The trash (5.00 MB), leftovers of removed apps (2.00 MB).")
        );
        // The screen it opens lists each kind, the trash unticked.
        let findings = crate::junk::phone_findings(&report.junk);
        let titles: Vec<&str> = findings.iter().map(|f| f.rule.title).collect();
        assert_eq!(
            titles,
            ["Thumbnail cache", "Trash", "Leftovers of removed apps"]
        );
        assert!(findings[0].ticked() && !findings[1].ticked() && !findings[2].ticked());
        assert_eq!(
            junk_confirm(&report.junk[2]),
            "Move 1 folder left by removed apps (2.00 MB) to the quarantine? Put back returns \
             them if an app turns out to need them."
        );
        let mut no_access = clean();
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
        let rows = rows(&AppsState::Ready(report.clone()), page(View::Media));
        assert_eq!(rows[0].detail, "1 app · 300 MB");
        let apps = of_kind(&rows, APP);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].subtitle, "12 files, mostly in Pictures/Big");
        assert_eq!(apps[0].detail, "300 MB");
        assert!(apps[0].tags.ends_with("300 MB of files"));

        let sheet = app_sheet(&report.apps[0], &report, options().abilities);
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
        let home = rows(&AppsState::Ready(report.clone()), page(View::Cache));
        assert_eq!(home[0].title, "Biggest caches");
        assert_eq!(home[0].detail, "2 apps · 340 MB");
        let apps = of_kind(&home, APP);
        assert_eq!(apps[0].detail, "300 MB");
        assert_eq!(apps[1].detail, "40.0 MB");

        let mut cleared = clean();
        cleared.cache_freed = Some(2 * MB);
        let home = rows(&AppsState::Ready(report), cleared);
        let card = home
            .iter()
            .find(|r| r.title.starts_with("App caches"))
            .unwrap();
        assert!(
            card.subtitle
                .starts_with("Android's cache cleaner freed 2.00 MB last time.")
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
        let home = rows(&AppsState::Ready(report.clone()), page(View::Growing));
        assert_eq!(home[0].detail, "1 app · +300 MB");
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
        let home = rows(&AppsState::Ready(report.clone()), options());
        assert!(of_kind(&home, CHART).is_empty());

        let sample = |day: u64| StorageSample {
            day,
            total: 1000 * MB,
            free: 500 * MB,
            apps: 200 * MB,
            files: 100 * MB,
        };
        report.storage = vec![sample(20_000), sample(20_010)];
        let home = rows(&AppsState::Ready(report), options());
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
