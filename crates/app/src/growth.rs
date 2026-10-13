//! How fast storage fills up: what grew over the last month, when a drive
//! will be full at that rate, and the shapes of the storage chart.
//!
//! Drives are sampled once a day into `drive-history.json` in FreeSize's
//! data folder; apps and the phone's storage breakdown are sampled into the
//! app history (see `history`).

use std::fs;
use std::path::PathBuf;

use freesize_core::{DiskSpace, format_size};
use serde::{Deserialize, Serialize};

use crate::history::{StorageSample, write_atomically};
use crate::insights::date;

const DAY: u64 = 24 * 60 * 60;
/// Growth is measured over about this many days.
pub const WINDOW: u64 = 30;
/// Less history than this says too little about growth.
const MIN_SPAN: u64 = 3;
/// A forecast needs this much history, and this many samples.
const FORECAST_SPAN: u64 = 7;
const FORECAST_SAMPLES: usize = 3;
/// Forecasts further out than this aren't shown.
const FORECAST_LIMIT: f64 = 2.0 * 365.0;
/// Daily samples kept per drive.
const DRIVE_SAMPLES: usize = 365;

/// How much something changed between two samples.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Change {
    pub bytes: i64,
    pub days: u64,
}

/// Compares the newest of `samples` (`(day, bytes)`, oldest first) with the
/// newest one at least [`WINDOW`] days older, or with the oldest if the
/// history is shorter than that. `None` with less than a few days of
/// history.
pub fn change(samples: &[(u64, u64)]) -> Option<Change> {
    let &(last_day, last) = samples.last()?;
    let &(base_day, base) = samples
        .iter()
        .rev()
        .find(|(day, _)| day + WINDOW <= last_day)
        .or(samples.first())?;
    let days = last_day - base_day;
    (days >= MIN_SPAN).then(|| Change {
        bytes: last as i64 - base as i64,
        days,
    })
}

/// "grew 1.20 GB in 30 days".
pub fn change_text(change: Change) -> String {
    let verb = if change.bytes < 0 { "shrank" } else { "grew" };
    format!(
        "{verb} {} in {} days",
        format_size(change.bytes.unsigned_abs()),
        change.days
    )
}

/// Like [`change_text`], but changes too small to matter on a drive of
/// `total` bytes are "hardly changed".
fn used_change_text(change: Change, total: u64) -> String {
    if change.bytes.unsigned_abs() < total / STEADY {
        format!("hardly changed in {} days", change.days)
    } else {
        change_text(change)
    }
}

/// Used space changing by less than 1/STEADY of the drive is steady.
const STEADY: u64 = 200;

/// Where used space is heading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Forecast {
    /// Average change of used space per day, in bytes.
    pub per_day: f64,
    /// Days until no space is left, if used space is growing.
    pub days_left: Option<f64>,
}

/// Fits a straight line through the used space (`(day, bytes)`, oldest
/// first) of the last [`WINDOW`] days and extends it until `free` runs out.
pub fn forecast(samples: &[(u64, u64)], free: u64) -> Option<Forecast> {
    let &(last_day, _) = samples.last()?;
    let recent: Vec<(f64, f64)> = samples
        .iter()
        .filter(|(day, _)| day + WINDOW >= last_day)
        .map(|&(day, used)| ((day + WINDOW - last_day) as f64, used as f64))
        .collect();
    if recent.len() < FORECAST_SAMPLES
        || recent[recent.len() - 1].0 - recent[0].0 < FORECAST_SPAN as f64
    {
        return None;
    }
    let n = recent.len() as f64;
    let mean_x = recent.iter().map(|p| p.0).sum::<f64>() / n;
    let mean_y = recent.iter().map(|p| p.1).sum::<f64>() / n;
    let (sxx, sxy) = recent.iter().fold((0.0, 0.0), |(sxx, sxy), &(x, y)| {
        (
            sxx + (x - mean_x).powi(2),
            sxy + (x - mean_x) * (y - mean_y),
        )
    });
    if sxx == 0.0 {
        return None;
    }
    let per_day = sxy / sxx;
    Some(Forecast {
        per_day,
        days_left: (per_day > 0.0).then(|| free as f64 / per_day),
    })
}

/// "about 3 weeks"; `None` when it's too far off to matter.
pub fn time_left(days: f64) -> Option<String> {
    Some(match days {
        d if d < 1.5 => "about a day".to_string(),
        d if d < 14.0 => format!("about {} days", d.round()),
        d if d < 60.0 => format!("about {} weeks", (d / 7.0).round()),
        d if d < 548.0 => format!("about {} months", (d / 30.4).round()),
        d if d <= FORECAST_LIMIT => "about a year and a half".to_string(),
        _ => return None,
    })
}

/// Full within a month: worth a warning color.
pub fn urgent(forecast: &Forecast) -> bool {
    forecast.days_left.is_some_and(|d| d < 30.0)
}

/// The phone storage chart: filled areas in a 1000 × 100 box (y grows
/// downwards), drawn over each other in this order so they stack.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Chart {
    /// Used space; the part above files is the system's.
    pub used: String,
    /// Apps and files.
    pub files: String,
    pub apps: String,
    pub start: String,
    pub end: String,
}

pub const CHART_WIDTH: f32 = 1000.0;
pub const CHART_HEIGHT: f32 = 100.0;

/// `None` until there are samples from at least two days.
pub fn chart(samples: &[StorageSample]) -> Option<Chart> {
    let (first, last) = (samples.first()?, samples.last()?);
    if last.day <= first.day {
        return None;
    }
    let total = samples.iter().map(|s| s.total).max().unwrap_or(0).max(1) as f32;
    let span = (last.day - first.day) as f32;
    let area = |value: &dyn Fn(&StorageSample) -> u64| {
        let mut path = format!("M 0 {CHART_HEIGHT}");
        for s in samples {
            let x = (s.day - first.day) as f32 / span * CHART_WIDTH;
            let y = CHART_HEIGHT - (value(s) as f32 / total).min(1.0) * CHART_HEIGHT;
            path.push_str(&format!(" L {x:.1} {y:.1}"));
        }
        path.push_str(&format!(" L {CHART_WIDTH} {CHART_HEIGHT} Z"));
        path
    };
    Some(Chart {
        used: area(&|s| s.total.saturating_sub(s.free)),
        files: area(&|s| s.apps + s.files),
        apps: area(&|s| s.apps),
        start: date(first.day * DAY),
        end: date(last.day * DAY),
    })
}

/// What the chart's caption says about used space.
pub fn storage_summary(samples: &[StorageSample]) -> String {
    let used: Vec<(u64, u64)> = samples
        .iter()
        .map(|s| (s.day, s.total.saturating_sub(s.free)))
        .collect();
    let Some(change) = change(&used) else {
        return "The chart fills in as FreeSize is used on more days.".into();
    };
    let total = samples.last().map_or(0, |s| s.total);
    let mut text = format!("Used space {}.", used_change_text(change, total));
    let free = samples.last().map_or(0, |s| s.free);
    if let Some(left) = forecast(&used, free)
        .and_then(|f| f.days_left)
        .and_then(time_left)
    {
        text.push_str(&format!(" At this rate the storage is full in {left}."));
    }
    text
}

/// Used space of every drive on the start screen, once a day.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveLog {
    #[serde(default)]
    pub drives: Vec<DriveRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriveRecord {
    pub path: String,
    /// `[day, used, total]`, oldest first.
    pub samples: Vec<[u64; 3]>,
}

impl DriveLog {
    /// Records `space` as the drive's size today, replacing an earlier
    /// sample from the same day.
    pub fn record(&mut self, path: &str, now: u64, space: DiskSpace) {
        let i = match self.drives.iter().position(|d| d.path == path) {
            Some(i) => i,
            None => {
                self.drives.push(DriveRecord {
                    path: path.into(),
                    samples: Vec::new(),
                });
                self.drives.len() - 1
            }
        };
        let samples = &mut self.drives[i].samples;
        let day = now / DAY;
        if samples.last().is_some_and(|s| s[0] == day) {
            samples.pop();
        }
        samples.push([day, space.used(), space.total]);
        if samples.len() > DRIVE_SAMPLES {
            samples.drain(..samples.len() - DRIVE_SAMPLES);
        }
    }

    /// "grew 3.10 GB in 30 days · full in about 2 months", and whether that
    /// is soon enough to warn about. `None` without enough history.
    pub fn trend(&self, path: &str, free: u64) -> Option<(String, bool)> {
        let drive = self.drives.iter().find(|d| d.path == path)?;
        // A drive of another size at this path (replaced, resized, a
        // different disk mounted) says nothing about this one.
        let total = drive.samples.last()?[2];
        let used: Vec<(u64, u64)> = drive
            .samples
            .iter()
            .filter(|s| s[2] == total)
            .map(|s| (s[0], s[1]))
            .collect();
        let change = change(&used)?;
        let mut text = capitalized(&used_change_text(change, total));
        let forecast = forecast(&used, free);
        if let Some(left) = forecast.and_then(|f| f.days_left).and_then(time_left) {
            text.push_str(&format!(" · full in {left}"));
        }
        Some((text, forecast.as_ref().is_some_and(urgent)))
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

const DRIVE_LOG: &str = "drive-history.json";

/// Where FreeSize keeps its own files. On desktops, DiskTree's folder from
/// before the rename is moved into place the first time.
pub fn data_dir() -> Option<PathBuf> {
    let dir = data_dir_named()?;
    #[cfg(not(target_os = "android"))]
    let dir = freesize_core::renamed::adopt_dir(dir);
    Some(dir)
}

fn data_dir_named() -> Option<PathBuf> {
    if crate::apps::demo_enabled() {
        return Some(std::env::temp_dir().join("freesize-demo"));
    }
    #[cfg(target_os = "android")]
    return crate::android::private_dir();
    #[cfg(windows)]
    return std::env::var_os("LOCALAPPDATA").map(|dir| PathBuf::from(dir).join("FreeSize"));
    #[cfg(target_os = "macos")]
    return std::env::home_dir().map(|home| home.join("Library/Application Support/FreeSize"));
    #[cfg(all(unix, not(target_os = "android"), not(target_os = "macos")))]
    return std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::home_dir().map(|home| home.join(".local/share")))
        .map(|dir| dir.join("freesize"));
    #[allow(unreachable_code)]
    None
}

/// Adds today's sample of each drive to the saved log and returns it.
pub fn record_drives(drives: &[(String, DiskSpace)], now: u64) -> DriveLog {
    let path = data_dir().map(|dir| dir.join(DRIVE_LOG));
    let mut log: DriveLog = path
        .as_ref()
        .and_then(|path| fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    for (drive, space) in drives {
        if space.total > 0 {
            log.record(drive, now, *space);
        }
    }
    if let Some(path) = path
        && let Ok(json) = serde_json::to_vec(&log)
    {
        // The trends just don't show if this fails.
        let _ = write_atomically(&path, &json);
    }
    log
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn change_over_the_window() {
        assert_eq!(change(&[]), None);
        assert_eq!(change(&[(100, 5)]), None);
        assert_eq!(change(&[(100, 5), (102, 9)]), None);
        // Shorter history: compared with the oldest sample.
        assert_eq!(
            change(&[(100, 5 * GB), (110, 6 * GB)]),
            Some(Change {
                bytes: GB as i64,
                days: 10
            })
        );
        // Longer: with the newest sample at least 30 days old.
        let samples = [(10, 1), (50, 2), (70, 4), (85, 100), (100, 10)];
        assert_eq!(change(&samples), Some(Change { bytes: 6, days: 30 }));
        assert_eq!(
            change_text(Change {
                bytes: -(GB as i64),
                days: 30
            }),
            "shrank 1.00 GB in 30 days"
        );
    }

    #[test]
    fn forecast_fits_a_line() {
        // 1 GB a day with 20 GB free: full in 20 days.
        let samples: Vec<(u64, u64)> = (0..10).map(|d| (1000 + d, 50 * GB + d * GB)).collect();
        let f = forecast(&samples, 20 * GB).unwrap();
        assert!((f.per_day - GB as f64).abs() < 1.0);
        assert!((f.days_left.unwrap() - 20.0).abs() < 0.01);
        assert!(urgent(&f));
        assert_eq!(time_left(20.0).as_deref(), Some("about 3 weeks"));
        assert_eq!(time_left(5000.0), None);

        // Shrinking: never full.
        let shrinking: Vec<(u64, u64)> = (0..10).map(|d| (d, 50 * GB - d * GB)).collect();
        assert_eq!(forecast(&shrinking, GB).unwrap().days_left, None);

        // Too little history.
        assert_eq!(forecast(&samples[..2], GB), None);
        assert_eq!(forecast(&[(0, 1), (1, 2), (2, 3)], GB), None);
    }

    fn sample(day: u64, used: u64) -> StorageSample {
        StorageSample {
            day,
            total: 100 * GB,
            free: (100 - used) * GB,
            apps: used / 2 * GB,
            files: used / 4 * GB,
        }
    }

    #[test]
    fn chart_shapes() {
        assert_eq!(chart(&[sample(10, 40)]), None);
        let c = chart(&[sample(10, 40), sample(20, 60)]).unwrap();
        assert_eq!(c.used, "M 0 100 L 0.0 60.0 L 1000.0 40.0 L 1000 100 Z");
        assert_eq!(c.apps, "M 0 100 L 0.0 80.0 L 1000.0 70.0 L 1000 100 Z");
        assert_eq!(c.start, "11 Jan 1970");
        assert_eq!(c.end, "21 Jan 1970");
        assert_eq!(
            storage_summary(&[sample(10, 40), sample(20, 60)]),
            "Used space grew 20.0 GB in 10 days."
        );
        let filling: Vec<StorageSample> = (0..10).map(|d| sample(d, 80 + d)).collect();
        assert_eq!(
            storage_summary(&filling),
            "Used space grew 9.00 GB in 9 days. At this rate the storage is full in about \
             11 days."
        );
        assert_eq!(
            storage_summary(&[sample(10, 40), sample(20, 40)]),
            "Used space hardly changed in 10 days."
        );
    }

    #[test]
    fn drive_log_keeps_one_sample_a_day() {
        let space = |used: u64| DiskSpace {
            total: 100 * GB,
            free: (100 - used) * GB,
        };
        let mut log = DriveLog::default();
        log.record("/", 10 * DAY, space(40));
        log.record("/", 10 * DAY + 60, space(41));
        assert_eq!(log.drives[0].samples, vec![[10, 41 * GB, 100 * GB]]);
        assert_eq!(log.trend("/", 59 * GB), None);
        for day in 11..=20 {
            log.record("/", day * DAY, space(41 + (day - 10) * 2));
        }
        let (text, warn) = log.trend("/", 39 * GB).unwrap();
        assert_eq!(text, "Grew 20.0 GB in 10 days · full in about 3 weeks");
        assert!(warn);
        assert_eq!(log.trend("/other", GB), None);

        // A bigger disk at the same path starts a new history.
        log.record(
            "/",
            21 * DAY,
            DiskSpace {
                total: 200 * GB,
                free: 100 * GB,
            },
        );
        assert_eq!(log.trend("/", 100 * GB), None);
    }
}
