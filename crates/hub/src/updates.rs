//! The Hub's own update check, with `FREESIZE_UPDATE_CHECK=on`: once a day
//! it asks GitHub for the newest release, shows it in Settings, and tells
//! devices running an older version in their poll answers. Devices still
//! verify the release's signature themselves before installing anything;
//! the Hub only points at it. Off by default: a self-hosted Hub doesn't
//! phone anywhere unless asked to.

use std::sync::Mutex;
use std::time::Duration;

use freesize_agent::update::{self, Channel};
use freesize_proto::LatestRelease;

use crate::AppState;

const EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// What the last check found.
#[derive(Default)]
pub struct Latest(Mutex<Option<LatestRelease>>);

impl Latest {
    pub fn get(&self) -> Option<LatestRelease> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set(&self, latest: Option<LatestRelease>) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = latest;
    }

    /// The release to tell a device about: the newest known one, when it's
    /// newer than what the device runs.
    pub fn for_version(&self, version: &str) -> Option<LatestRelease> {
        self.get()
            .filter(|latest| update::newer(&latest.version, version))
    }
}

/// Asks GitHub (blocking; run it on a blocking thread).
pub fn check_now(channel: Channel) -> Result<Option<LatestRelease>, String> {
    let transport = freesize_agent::UreqTransport::new();
    Ok(
        update::check(&transport, channel, update::CURRENT)?.map(|release| LatestRelease {
            version: release.version,
            url: release.url,
        }),
    )
}

/// Runs at start and then once a day, for as long as the Hub runs.
pub async fn run(state: AppState) {
    if !state.config.update_check() {
        return;
    }
    let mut every = tokio::time::interval(EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let channel = Channel::from_id(&state.config.update_channel);
        match tokio::task::spawn_blocking(move || check_now(channel)).await {
            Ok(Ok(latest)) => {
                match &latest {
                    Some(l) => tracing::info!("FreeSize {} is out: {}", l.version, l.url),
                    None => tracing::info!("FreeSize {} is the newest release", update::CURRENT),
                }
                state.latest.set(latest);
            }
            Ok(Err(err)) => tracing::warn!("checking for a newer FreeSize failed: {err}"),
            Err(err) => tracing::warn!("checking for a newer FreeSize failed: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_older_devices_are_told() {
        let latest = Latest::default();
        assert_eq!(latest.for_version("0.8.8"), None);
        latest.set(Some(LatestRelease {
            version: "0.9.1".into(),
            url: "u".into(),
        }));
        assert!(latest.for_version("0.8.8").is_some());
        assert!(latest.for_version("FreeSize app 0.9.0").is_some());
        assert_eq!(latest.for_version("0.9.1"), None);
        assert_eq!(latest.for_version("0.10.0"), None);
        assert_eq!(latest.for_version(""), None);
    }
}
