//! Turns raw app facts into advice: where an app came from and whether it can
//! be installed again, which apps look unused, and what to know before
//! removing one.

use crate::apps::AppUsage;

const DAY: u64 = 24 * 60 * 60;
/// Apps not opened for this long count as unused.
pub const UNUSED_AFTER: u64 = 90 * DAY;
/// Apps installed more recently than this aren't called "never opened" yet.
const GRACE: u64 = 14 * DAY;
/// Smaller caches aren't worth clearing by hand.
const CACHE_WORTH_LISTING: u64 = 10 * 1024 * 1024;
/// Apps that grew this much in a month are growing fast.
pub const GROWING: u64 = 100 * 1024 * 1024;

/// Where an app was installed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Play,
    /// Another app store, by name.
    Store(&'static str),
    /// Installed by opening a downloaded APK file.
    Downloaded,
    /// Came with the phone.
    Preinstalled,
    /// No installer recorded, usually installed from a computer with adb.
    Unknown,
}

/// Installers that can download the app again.
const STORES: &[(&str, &str)] = &[
    ("com.aurora.store", "Aurora Store"),
    ("com.sec.android.app.samsungapps", "Galaxy Store"),
    ("com.amazon.venezia", "Amazon Appstore"),
    ("org.fdroid.fdroid", "F-Droid"),
    ("org.fdroid.basic", "F-Droid"),
    ("com.looker.droidify", "Droid-ify"),
    ("com.machiav3lli.fdroid", "Neo Store"),
    ("dev.imranr.obtainium", "Obtainium"),
    ("com.huawei.appmarket", "AppGallery"),
    ("com.xiaomi.mipicks", "GetApps"),
    ("com.heytap.market", "App Market"),
];

impl Source {
    pub fn of(app: &AppUsage) -> Source {
        if app.system {
            return Source::Preinstalled;
        }
        match app.installer.as_deref() {
            // The Play Store, or Google's setup and restore tools, which
            // install from it.
            Some(
                "com.android.vending"
                | "com.google.android.feedback"
                | "com.google.android.gms"
                | "com.google.android.apps.restore",
            ) => Source::Play,
            None | Some("com.android.shell") => Source::Unknown,
            Some(installer) => STORES
                .iter()
                .find(|(package, _)| *package == installer)
                .map_or(Source::Downloaded, |(_, name)| Source::Store(name)),
        }
    }

    /// Whether the app can be downloaded again after uninstalling it.
    pub fn reinstallable(self) -> bool {
        matches!(self, Source::Play | Source::Store(_))
    }

    /// Short form for tags: "Play Store", "Downloaded file".
    pub fn tag(self) -> &'static str {
        match self {
            Source::Play => "Play Store",
            Source::Store(name) => name,
            Source::Downloaded => "Downloaded file",
            Source::Preinstalled => "Preinstalled",
            Source::Unknown => "Unknown source",
        }
    }
}

/// Whether FreeSize may suggest removing the app. System apps, apps without
/// an icon (keyboards, services, plugins), apps working in the background
/// for the user (default apps, accessibility, device admin) and archived
/// apps are left out of the suggestions.
pub fn suggestable(app: &AppUsage) -> bool {
    !app.system && app.has_launcher && app.protected.is_none() && !app.archived
}

/// The groups the home screen can show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum View {
    #[default]
    All,
    Unused,
    NeverOpened,
    NotFromStore,
    /// Apps by the files they saved to shared storage.
    Media,
    /// Apps by cache size.
    Cache,
    /// Apps by how much they grew over the last month.
    Growing,
    Removed,
}

impl View {
    pub const ALL: [View; 8] = [
        View::All,
        View::Unused,
        View::NeverOpened,
        View::NotFromStore,
        View::Media,
        View::Cache,
        View::Growing,
        View::Removed,
    ];

    pub fn id(self) -> &'static str {
        match self {
            View::All => "all",
            View::Unused => "unused",
            View::NeverOpened => "never-opened",
            View::NotFromStore => "not-from-store",
            View::Media => "media",
            View::Cache => "cache",
            View::Growing => "growing",
            View::Removed => "removed",
        }
    }

    pub fn from_id(id: &str) -> Option<View> {
        View::ALL.into_iter().find(|view| view.id() == id)
    }

    pub fn title(self) -> &'static str {
        match self {
            View::All => "All apps",
            View::Unused => "Not opened in 3 months",
            View::NeverOpened => "Never opened",
            View::NotFromStore => "Not from an app store",
            View::Media => "Files saved by apps",
            View::Cache => "Biggest caches",
            View::Growing => "Growing fast",
            View::Removed => "Removed apps",
        }
    }

    /// Whether `app` belongs in this group. `usage_known` says whether
    /// Android reported any usage history at all; without it nothing can be
    /// called unused.
    pub fn contains(self, app: &AppUsage, usage_known: bool, now: u64) -> bool {
        match self {
            View::All => true,
            View::Unused => {
                usage_known
                    && suggestable(app)
                    && app
                        .last_opened
                        .is_some_and(|t| now.saturating_sub(t) >= UNUSED_AFTER)
            }
            View::NeverOpened => {
                usage_known
                    && suggestable(app)
                    && app.last_opened.is_none()
                    && app
                        .installed_at
                        .is_none_or(|t| now.saturating_sub(t) >= GRACE)
            }
            View::NotFromStore => !app.own && !Source::of(app).reinstallable() && !app.system,
            View::Cache => app.cache_bytes >= CACHE_WORTH_LISTING,
            View::Growing => growing(app),
            // Listed from the report's media and history, not per app.
            View::Media | View::Removed => false,
        }
    }
}

/// Grew by [`GROWING`] or more over the last month.
pub fn growing(app: &AppUsage) -> bool {
    app.growth.is_some_and(|g| g.bytes >= GROWING as i64)
}

/// What to know about a few well-known apps before clearing or removing them.
pub fn tip(package: &str) -> Option<&'static str> {
    Some(match package {
        "org.thoughtcrime.securesms" => {
            "Signal keeps your messages only on this phone. Clearing its storage or \
             uninstalling it deletes them, unless you've set up a backup in Signal's settings."
        }
        "com.whatsapp" | "com.whatsapp.w4b" => {
            "Chats come back after a reinstall only from a Google Drive backup. To free space \
             safely, use WhatsApp's Settings → Storage and data → Manage storage, which lists \
             the biggest chats and files."
        }
        "org.telegram.messenger" | "org.telegram.messenger.web" => {
            "Telegram keeps chats online, so most of its space is cached media. Settings → Data \
             and storage → Storage usage clears it."
        }
        "com.google.android.aicore" => {
            "Android's on-device AI model (Gemini Nano). Clearing it only makes Android download \
             it again."
        }
        "com.spotify.music"
        | "com.google.android.apps.youtube.music"
        | "com.google.android.youtube"
        | "com.netflix.mediaclient"
        | "com.amazon.avod.thirdpartyclient"
        | "com.disney.disneyplus" => {
            "Most of this is probably offline downloads. Remove the ones you don't need inside \
             the app."
        }
        "com.google.android.apps.photos" => {
            "“Free up space” in the Photos app (under your profile picture) removes copies that \
             are already backed up."
        }
        "com.google.android.apps.maps" => {
            "Offline maps can take several GB: Maps → your profile picture → Offline maps."
        }
        _ => return None,
    })
}

/// The developer-declared category, if any.
pub fn category(id: i32) -> Option<&'static str> {
    Some(match id {
        0 => "Game",
        1 => "Audio",
        2 => "Video",
        3 => "Photos and images",
        4 => "Social",
        5 => "News",
        6 => "Maps",
        7 => "Productivity",
        8 => "Accessibility",
        _ => return None,
    })
}

/// "today", "3 days ago", "5 months ago".
pub fn ago(now: u64, then: u64) -> String {
    let days = now.saturating_sub(then) / DAY;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=13 => format!("{days} days ago"),
        14..=59 => format!("{} weeks ago", days / 7),
        60..=729 => format!("{} months ago", days / 30),
        _ => format!("{} years ago", days / 365),
    }
}

/// "12 Mar 2026" (UTC).
pub fn date(secs: u64) -> String {
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = (secs / DAY) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    format!("{day} {} {year}", MONTHS[(month - 1) as usize])
}

/// "2 h 10 min", "35 min".
pub fn duration(secs: u64) -> String {
    let minutes = secs / 60;
    match minutes {
        0 => "under a minute".into(),
        1..=59 => format!("{minutes} min"),
        _ if minutes.is_multiple_of(60) => format!("{} h", minutes / 60),
        _ => format!("{} h {} min", minutes / 60, minutes % 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_790_000_000; // Sep 2026

    fn app() -> AppUsage {
        AppUsage {
            package: "p".into(),
            label: "App".into(),
            installer: Some("com.android.vending".into()),
            has_launcher: true,
            installed_at: Some(NOW - 400 * DAY),
            last_opened: Some(NOW - DAY),
            ..AppUsage::default()
        }
    }

    #[test]
    fn sources() {
        let mut a = app();
        assert_eq!(Source::of(&a), Source::Play);
        a.installer = Some("com.google.android.apps.restore".into());
        assert_eq!(Source::of(&a), Source::Play);
        a.installer = Some("org.fdroid.fdroid".into());
        assert_eq!(Source::of(&a), Source::Store("F-Droid"));
        a.installer = Some("com.google.android.packageinstaller".into());
        assert_eq!(Source::of(&a), Source::Downloaded);
        a.installer = None;
        assert_eq!(Source::of(&a), Source::Unknown);
        a.system = true;
        assert_eq!(Source::of(&a), Source::Preinstalled);
        assert!(Source::Store("F-Droid").reinstallable());
        assert!(!Source::Downloaded.reinstallable());
    }

    #[test]
    fn unused_needs_old_last_use_and_usage_history() {
        let mut a = app();
        assert!(!View::Unused.contains(&a, true, NOW));
        a.last_opened = Some(NOW - 120 * DAY);
        assert!(View::Unused.contains(&a, true, NOW));
        assert!(!View::Unused.contains(&a, false, NOW));
        assert!(!View::NeverOpened.contains(&a, true, NOW));
    }

    #[test]
    fn never_opened_skips_new_installs() {
        let mut a = app();
        a.last_opened = None;
        assert!(View::NeverOpened.contains(&a, true, NOW));
        a.installed_at = Some(NOW - 3 * DAY);
        assert!(!View::NeverOpened.contains(&a, true, NOW));
    }

    #[test]
    fn suggestions_leave_out_apps_working_in_the_background() {
        let mut a = app();
        a.last_opened = Some(NOW - 300 * DAY);
        for change in [
            |a: &mut AppUsage| a.system = true,
            |a: &mut AppUsage| a.has_launcher = false,
            |a: &mut AppUsage| a.protected = Some("a keyboard".into()),
            |a: &mut AppUsage| a.archived = true,
        ] {
            let mut b = a.clone();
            change(&mut b);
            assert!(!View::Unused.contains(&b, true, NOW));
        }
    }

    #[test]
    fn not_from_store_skips_store_and_system_apps_and_itself() {
        let mut a = app();
        assert!(!View::NotFromStore.contains(&a, true, NOW));
        a.installer = Some("com.android.chrome".into());
        assert!(View::NotFromStore.contains(&a, true, NOW));
        a.own = true;
        assert!(!View::NotFromStore.contains(&a, true, NOW));
    }

    #[test]
    fn view_ids_round_trip() {
        for view in View::ALL {
            assert_eq!(View::from_id(view.id()), Some(view));
        }
    }

    #[test]
    fn formatting() {
        assert_eq!(ago(NOW, NOW - 3600), "today");
        assert_eq!(ago(NOW, NOW - 5 * DAY), "5 days ago");
        assert_eq!(ago(NOW, NOW - 20 * DAY), "2 weeks ago");
        assert_eq!(ago(NOW, NOW - 100 * DAY), "3 months ago");
        assert_eq!(ago(NOW, NOW - 800 * DAY), "2 years ago");
        assert_eq!(date(0), "1 Jan 1970");
        assert_eq!(date(1_772_323_200), "1 Mar 2026");
        assert_eq!(date(951_782_400), "29 Feb 2000");
        assert_eq!(duration(30), "under a minute");
        assert_eq!(duration(35 * 60), "35 min");
        assert_eq!(duration(2 * 3600), "2 h");
        assert_eq!(duration(2 * 3600 + 600), "2 h 10 min");
    }
}
