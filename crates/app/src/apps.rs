//! Space used by installed apps and by the system.
//!
//! Android keeps every app's private data (and its folders under
//! `Android/data` and `Android/obb`) out of reach of other apps' file access,
//! so a file scan can't see most of what fills a phone. Android does report
//! the sizes through `StorageStatsManager` once the user grants "Usage
//! access"; that is what this module surfaces. Other platforms have no
//! equivalent, so there it reports itself as unsupported.

use disktree_core::DiskSpace;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppUsage {
    pub package: String,
    pub label: String,
    /// APK, compiled code and native libraries.
    pub code_bytes: u64,
    /// Private data, including caches and the app's folders in shared storage.
    pub data_bytes: u64,
    /// The part of `data_bytes` that is cache, which the app can rebuild.
    pub cache_bytes: u64,
    /// Preinstalled and never updated, so its code lives on the system
    /// partition rather than in user storage.
    pub system_image: bool,
}

impl AppUsage {
    /// Space the app takes in user storage. The code of preinstalled apps
    /// sits on the system partition, so only their data counts, which is how
    /// Android's own Settings attributes it.
    pub fn size(&self) -> u64 {
        if self.system_image {
            self.data_bytes
        } else {
            self.code_bytes + self.data_bytes
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AppReport {
    /// Largest first.
    pub apps: Vec<AppUsage>,
    /// Shared storage that belongs to no app: the user's own files.
    pub shared_files: Option<u64>,
    /// The capacity printed on the box ("128 GB"), larger than user storage
    /// because the OS has partitions of its own.
    pub advertised_total: Option<u64>,
}

/// How user storage divides up. `apps + files + system + free == total`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Breakdown {
    pub total: u64,
    pub free: u64,
    pub apps: u64,
    /// Part of `apps` that is cache.
    pub cache: u64,
    pub files: u64,
    /// Everything else: OS data, other users, temporary files.
    pub system: u64,
    /// Space the OS uses outside user storage (advertised minus `total`).
    pub outside: u64,
}

impl AppReport {
    pub fn breakdown(&self, space: DiskSpace) -> Breakdown {
        let used = space.used();
        let apps = self.apps.iter().map(AppUsage::size).sum::<u64>().min(used);
        let cache = self
            .apps
            .iter()
            .map(|a| a.cache_bytes)
            .sum::<u64>()
            .min(apps);
        let files = self.shared_files.unwrap_or(0).min(used - apps);
        Breakdown {
            total: space.total,
            free: space.free,
            apps,
            cache,
            files,
            system: used - apps - files,
            outside: self
                .advertised_total
                .map_or(0, |advertised| advertised.saturating_sub(space.total)),
        }
    }
}

/// Whether app storage can be measured on this platform.
pub fn supported() -> bool {
    cfg!(target_os = "android") || demo::enabled()
}

/// Whether the user has allowed reading app storage.
pub fn has_access() -> bool {
    if demo::enabled() {
        return demo::access_granted();
    }
    platform::has_access()
}

/// Asks the user to allow reading app storage.
pub fn request_access() {
    platform::request_access();
}

/// Measures every installed app, largest first. Slow, so call it off the UI
/// thread.
pub fn collect() -> Result<AppReport, String> {
    let mut report = if demo::enabled() {
        demo::report()
    } else {
        platform::collect()?
    };
    report
        .apps
        .sort_by(|a, b| b.size().cmp(&a.size()).then_with(|| a.label.cmp(&b.label)));
    Ok(report)
}

/// Opens the app's system settings page (clear cache or data, uninstall).
pub fn open(package: &str) {
    platform::open(package);
}

#[cfg(target_os = "android")]
mod platform {
    pub use crate::android::{
        collect_app_report as collect, has_usage_access as has_access, open_app_settings as open,
        request_usage_access as request_access,
    };
}

#[cfg(not(target_os = "android"))]
mod platform {
    use super::AppReport;

    pub fn has_access() -> bool {
        false
    }

    pub fn request_access() {}

    pub fn collect() -> Result<AppReport, String> {
        Err("App storage is only available on Android".into())
    }

    pub fn open(_package: &str) {}
}

/// Sample data for working on the UI without a phone: debug builds started
/// with `DISKTREE_FAKE_APPS=1` show it on the home screen, and
/// `DISKTREE_FAKE_APPS=no-access` shows the Usage access prompt.
mod demo {
    use super::{AppReport, AppUsage};

    pub fn enabled() -> bool {
        cfg!(debug_assertions) && std::env::var_os("DISKTREE_FAKE_APPS").is_some()
    }

    pub fn access_granted() -> bool {
        std::env::var("DISKTREE_FAKE_APPS").as_deref() != Ok("no-access")
    }

    pub fn report() -> AppReport {
        const MB: u64 = 1024 * 1024;
        let app = |label: &str, package: &str, code: u64, data: u64, cache: u64| AppUsage {
            package: package.into(),
            label: label.into(),
            code_bytes: code * MB,
            data_bytes: data * MB,
            cache_bytes: cache * MB,
            system_image: false,
        };
        AppReport {
            apps: vec![
                app("WhatsApp", "com.whatsapp", 120, 18_400, 650),
                app("Spotify", "com.spotify.music", 160, 9_800, 7_900),
                app("Chrome", "com.android.chrome", 410, 3_100, 2_300),
                app(
                    "Call of Duty",
                    "com.activision.callofduty.shooter",
                    2_900,
                    6_200,
                    140,
                ),
                app("Netflix", "com.netflix.mediaclient", 95, 4_700, 80),
                app(
                    "Google Photos",
                    "com.google.android.apps.photos",
                    240,
                    1_900,
                    1_100,
                ),
            ],
            shared_files: Some(4_400 * MB),
            advertised_total: Some(128 * 1024 * MB),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    fn app(code: u64, data: u64, cache: u64, system_image: bool) -> AppUsage {
        AppUsage {
            package: "p".into(),
            label: "l".into(),
            code_bytes: code,
            data_bytes: data,
            cache_bytes: cache,
            system_image,
        }
    }

    #[test]
    fn preinstalled_apps_only_count_their_data() {
        assert_eq!(app(5, 10, 1, false).size(), 15);
        assert_eq!(app(5, 10, 1, true).size(), 10);
    }

    #[test]
    fn breakdown_adds_up_to_total() {
        let report = AppReport {
            apps: vec![
                app(2 * GB, 50 * GB, 5 * GB, false),
                app(GB, 8 * GB, GB, true),
            ],
            shared_files: Some(4 * GB),
            advertised_total: Some(128 * GB),
        };
        let space = DiskSpace {
            total: 109 * GB,
            free: 21 * GB,
        };
        let b = report.breakdown(space);
        assert_eq!(b.apps, 60 * GB);
        assert_eq!(b.cache, 6 * GB);
        assert_eq!(b.files, 4 * GB);
        assert_eq!(b.system, 24 * GB);
        assert_eq!(b.outside, 19 * GB);
        assert_eq!(b.apps + b.files + b.system + b.free, b.total);
    }

    #[test]
    fn breakdown_never_exceeds_used_space() {
        let report = AppReport {
            apps: vec![app(0, 200 * GB, 0, false)],
            shared_files: Some(50 * GB),
            advertised_total: None,
        };
        let b = report.breakdown(DiskSpace {
            total: 100 * GB,
            free: 10 * GB,
        });
        assert_eq!(b.apps, 90 * GB);
        assert_eq!(b.files, 0);
        assert_eq!(b.system, 0);
        assert_eq!(b.outside, 0);
    }
}
