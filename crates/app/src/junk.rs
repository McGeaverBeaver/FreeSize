//! The Clean junk screen on desktops: what the known places hold, by
//! category, ticked where it's safe, and the result afterwards. The
//! finding and deleting is `freesize_core::junk`; "check first" items go to
//! this device's quarantine. An optional daily or weekly check says on the
//! home screen when junk piles up; nothing is ever cleaned on its own.

use std::collections::HashSet;
use std::path::PathBuf;

use freesize_agent::junk as cleaner;
use freesize_core::format_size;
use freesize_core::junk::{self, Category, Finding, Safety};
use serde::{Deserialize, Serialize};

use crate::home::{count, heading, notice};
use crate::{HomeRow, JunkRow};

/// Action ids of the home screen's Clean junk cards.
pub const FIND: &str = "junk-find";
pub const OPEN: &str = "junk-open";
pub const REMIND: &str = "junk-remind";

/// Row kinds of the Clean junk screen.
const CATEGORY: i32 = 0;
const RULE: i32 = 1;
const ITEM: i32 = 2;
const NOTE: i32 = 3;

/// Item rows shown per rule before "and N more".
const ITEMS_SHOWN: usize = 60;

/// The reminder setting, in `junk.json` in FreeSize's data folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    /// `off`, `daily` or `weekly`.
    #[serde(default)]
    pub remind: String,
    /// When the last check ran (seconds since 1970).
    #[serde(default)]
    pub checked: i64,
    /// What it found.
    #[serde(default)]
    pub found: u64,
    #[serde(default)]
    pub safe: u64,
}

impl Settings {
    /// Seconds between checks, or none when off.
    fn every(&self) -> Option<i64> {
        match self.remind.as_str() {
            "daily" => Some(24 * 60 * 60),
            "weekly" => Some(7 * 24 * 60 * 60),
            _ => None,
        }
    }

    /// The next setting round the button: off, weekly, daily.
    pub fn next_remind(&self) -> &'static str {
        match self.remind.as_str() {
            "weekly" => "daily",
            "daily" => "off",
            _ => "weekly",
        }
    }

    /// Whether a quiet check is due now.
    pub fn due(&self, now: i64) -> bool {
        self.every()
            .is_some_and(|every| now - self.checked >= every)
    }
}

/// What cleaning did, for the screen's top line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub freed: u64,
    pub quarantined: u64,
    pub skipped: Vec<junk::Skipped>,
    /// The part of `freed` that comes back within days.
    pub refills: u64,
    /// Rules that couldn't be cleaned, with why.
    pub failed: Vec<(String, String)>,
}

impl Outcome {
    pub fn summary(&self) -> String {
        let mut text = format!("Freed {}.", format_size(self.freed));
        if self.quarantined > 0 {
            text.push_str(&format!(
                " Moved {} to the quarantine; it's freed when you empty it.",
                format_size(self.quarantined)
            ));
        }
        if !self.skipped.is_empty() {
            text.push_str(&format!(
                " Skipped {} in use.",
                match self.skipped.len() {
                    1 => "1 file".to_string(),
                    n => format!("{n} files"),
                }
            ));
        }
        if self.refills > 0 {
            text.push_str(&format!(
                " {} of that refills within days of normal use.",
                format_size(self.refills)
            ));
        }
        for (title, why) in &self.failed {
            text.push_str(&format!(" {title}: {why}."));
        }
        text
    }
}

/// The screen's state.
#[derive(Default)]
pub struct State {
    pub settings: Settings,
    file: Option<PathBuf>,
    /// What the last look found; empty before one.
    pub findings: Vec<Finding>,
    /// Per finding, whether it's ticked.
    pub ticked: Vec<bool>,
    /// Findings whose items are shown.
    pub open: HashSet<usize>,
    /// The last clean's result, shown until the next look.
    pub outcome: Option<Outcome>,
    /// A look has run since the app started.
    pub looked: bool,
}

impl State {
    pub fn new(data_dir: Option<PathBuf>) -> State {
        let file = data_dir.map(|d| d.join("junk.json"));
        let settings = file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        State {
            settings,
            file,
            ..State::default()
        }
    }

    pub fn save(&self) {
        if let Some(file) = &self.file
            && let Ok(text) = serde_json::to_string_pretty(&self.settings)
        {
            if let Some(dir) = file.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(file, text);
        }
    }

    /// Takes a fresh look's findings: ticks follow safety, open rules
    /// stay open.
    pub fn found(&mut self, findings: Vec<Finding>, now: i64) {
        self.ticked = findings.iter().map(Finding::ticked).collect();
        self.open.retain(|&i| i < findings.len());
        self.settings.checked = now;
        self.settings.found = junk::total(&findings);
        self.settings.safe = junk::safe_total(&findings);
        self.findings = findings;
        self.looked = true;
        self.save();
    }

    /// Ticks or unticks a rule; only what's safe and not in use can be.
    pub fn toggle(&mut self, index: usize) {
        if let (Some(finding), Some(ticked)) =
            (self.findings.get(index), self.ticked.get_mut(index))
            && finding.ticked()
        {
            *ticked = !*ticked;
        }
    }

    pub fn toggle_open(&mut self, index: usize) {
        if !self.open.remove(&index) {
            self.open.insert(index);
        }
    }

    /// The findings ticked now.
    pub fn chosen(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .zip(&self.ticked)
            .filter(|(_, t)| **t)
            .map(|(f, _)| f)
            .collect()
    }

    pub fn chosen_bytes(&self) -> u64 {
        self.chosen().iter().map(|f| f.bytes).sum()
    }

    /// The question before cleaning: what's deleted for good and why, and
    /// what goes to the quarantine.
    pub fn confirm_text(&self) -> String {
        let chosen = self.chosen();
        let deleted: u64 = chosen
            .iter()
            .filter(|f| f.rule.safety == Safety::Safe)
            .map(|f| f.bytes)
            .sum();
        let aside: u64 = chosen
            .iter()
            .filter(|f| f.rule.safety == Safety::CheckFirst)
            .map(|f| f.bytes)
            .sum();
        let refills: u64 = chosen
            .iter()
            .filter(|f| f.rule.refills)
            .map(|f| f.bytes)
            .sum();
        let mut text = String::new();
        if deleted > 0 {
            text.push_str(&format!(
                "Delete {} for good? Caches and temporary files are deleted outright: their \
                 apps make them again when needed, and in a quarantine they'd free nothing.",
                format_size(deleted)
            ));
        }
        if aside > 0 {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!(
                "{} goes to the quarantine, where Put back restores it.",
                format_size(aside)
            ));
        }
        if refills > 0 {
            text.push_str(&format!(
                " {} of this refills within days of normal use.",
                format_size(refills)
            ));
        }
        text
    }

    /// The rows of the screen.
    pub fn rows(&self, busy: bool) -> Vec<JunkRow> {
        let mut rows = Vec::new();
        if let Some(outcome) = &self.outcome {
            rows.push(JunkRow {
                kind: NOTE,
                title: "Cleaned".into(),
                subtitle: outcome.summary().into(),
                ..Default::default()
            });
        }
        if busy {
            rows.push(JunkRow {
                kind: NOTE,
                title: "Looking in the known places…".into(),
                subtitle: "Temporary files, caches, the trash and old downloads. This takes a \
                           few seconds; nothing is deleted."
                    .into(),
                ..Default::default()
            });
            return rows;
        }
        if self.findings.is_empty() {
            if self.looked {
                rows.push(JunkRow {
                    kind: NOTE,
                    title: "Nothing to clean".into(),
                    subtitle: "The known places are empty, or everything in them is in use.".into(),
                    ..Default::default()
                });
            }
            return rows;
        }
        for category in Category::ALL {
            let in_category: Vec<(usize, &Finding)> = self
                .findings
                .iter()
                .enumerate()
                .filter(|(_, f)| f.rule.category == category)
                .collect();
            if in_category.is_empty() {
                continue;
            }
            let total: u64 = in_category.iter().map(|(_, f)| f.bytes).sum();
            rows.push(JunkRow {
                kind: CATEGORY,
                title: category.title().into(),
                subtitle: category.blurb().into(),
                detail: format_size(total).into(),
                ..Default::default()
            });
            for (index, finding) in in_category {
                let mut subtitle = finding.rule.blurb.to_string();
                if finding.rule.refills {
                    subtitle.push_str(" Refills within days.");
                }
                let why = finding.why_not();
                if !why.is_empty() {
                    subtitle.push(' ');
                    subtitle.push_str(&why);
                }
                let action = match finding.rule.safety {
                    Safety::UseTool => match tool_url(finding.rule.tool) {
                        Some(_) => "Open settings",
                        None => "Show command",
                    },
                    _ => "",
                };
                rows.push(JunkRow {
                    kind: RULE,
                    id: index as i32,
                    title: finding.rule.title.into(),
                    subtitle: subtitle.into(),
                    detail: format!(
                        "{} · {}",
                        format_size(finding.bytes),
                        count(finding.items.len(), "item", "items")
                    )
                    .into(),
                    ticked: self.ticked.get(index).copied().unwrap_or(false),
                    enabled: finding.ticked(),
                    open: self.open.contains(&index),
                    action: action.into(),
                });
                if self.open.contains(&index) {
                    let mut listed = 0;
                    for (item_index, item) in finding.items.iter().enumerate() {
                        for path in item.paths() {
                            if listed == ITEMS_SHOWN {
                                break;
                            }
                            listed += 1;
                            rows.push(JunkRow {
                                kind: ITEM,
                                id: item_id(index, item_index),
                                title: path.to_string_lossy().into_owned().into(),
                                subtitle: item.note.clone().into(),
                                detail: if item.bytes > 0 || item.files > 0 {
                                    format_size(item.bytes).into()
                                } else {
                                    "empty".into()
                                },
                                action: if crate::actions::CAN_REVEAL {
                                    "Show".into()
                                } else {
                                    "".into()
                                },
                                ..Default::default()
                            });
                        }
                    }
                    let all: usize = finding.items.iter().map(|i| i.paths().len()).sum();
                    if all > listed {
                        rows.push(JunkRow {
                            kind: ITEM,
                            id: -1,
                            title: format!("… and {} more", all - listed).into(),
                            ..Default::default()
                        });
                    }
                }
            }
        }
        rows
    }

    /// The line under the screen's title.
    pub fn summary(&self) -> String {
        if self.findings.is_empty() {
            return String::new();
        }
        format!(
            "{} found; {} ticked. Tick what goes, open a rule to see its paths.",
            format_size(junk::total(&self.findings)),
            format_size(self.chosen_bytes())
        )
    }

    /// The path of an item row, for showing it.
    pub fn item_path(&self, id: i32) -> Option<PathBuf> {
        let (finding, item) = (id / 10_000, id % 10_000);
        self.findings
            .get(usize::try_from(finding).ok()?)?
            .items
            .get(usize::try_from(item).ok()?)
            .and_then(|i| i.paths().into_iter().next())
    }

    /// The home screen's Clean junk cards.
    pub fn home_rows(&self, now: i64) -> Vec<HomeRow> {
        let remind = match self.settings.remind.as_str() {
            "daily" => "Checks once a day and says here when junk piles up.",
            "weekly" => "Checks once a week and says here when junk piles up.",
            _ => {
                "Doesn't check on its own. Turn the reminder on and FreeSize looks once a \
                  week or once a day and says here when junk piles up; nothing is cleaned \
                  without you."
            }
        };
        let remind_button = match self.settings.next_remind() {
            "daily" => "Daily",
            "weekly" => "Weekly",
            _ => "Turn off",
        };
        let mut rows = vec![heading("Clean junk", "", Some((remind_button, REMIND)))];
        if self.settings.checked > 0 && self.settings.found > 0 {
            rows.push(notice(
                &format!(
                    "{} of junk to review, {} safe to clean",
                    format_size(self.settings.found),
                    format_size(self.settings.safe)
                ),
                &format!(
                    "Found {}. Caches, temporary files, the trash and old downloads, by \
                     category, each saying what cleaning it means.",
                    crate::insights::ago(now.max(0) as u64, self.settings.checked.max(0) as u64)
                ),
                Some(("Review", OPEN)),
            ));
        } else {
            rows.push(notice(
                "Find junk in the known places",
                "Temporary files, crash dumps, browser and app caches, shader caches, build \
                 caches, the trash and old downloads. It takes seconds and needs no scan. \
                 Nothing is deleted until you tick it and confirm.",
                Some(("Find junk", FIND)),
            ));
        }
        rows.push(notice("Reminder", remind, None));
        rows
    }
}

fn item_id(finding: usize, item: usize) -> i32 {
    (finding * 10_000 + item.min(9_999)) as i32
}

/// A settings page the tool's text names, as something `open_url` opens.
pub fn tool_url(tool: &str) -> Option<&'static str> {
    if tool.starts_with("Settings → System → Storage") {
        Some("ms-settings:storagesense")
    } else if tool.starts_with("System Settings → General → Software Update") {
        Some("x-apple.systempreferences:com.apple.Software-Update-Settings.extension")
    } else {
        None
    }
}

/// Cleans the chosen findings: safe ones deleted, "check first" ones into
/// this device's quarantine. Slow for big folders, so it runs off the UI
/// thread.
pub fn clean(chosen: Vec<Finding>) -> Outcome {
    let mut outcome = Outcome::default();
    for finding in &chosen {
        let title = finding.rule.title;
        let aside = |paths: &[PathBuf]| -> Result<u64, String> {
            let (bytes, errors) = crate::quarantine::delete_all(paths, title);
            match errors.into_iter().next() {
                Some(err) if bytes == 0 => Err(err),
                _ => Ok(bytes),
            }
        };
        match cleaner::clean(finding, &aside) {
            Ok(done) => {
                if finding.rule.refills {
                    outcome.refills += done.freed;
                }
                outcome.freed += done.freed;
                outcome.quarantined += done.quarantined;
                outcome.skipped.extend(done.skipped);
            }
            Err(err) => outcome.failed.push((title.to_string(), err)),
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use freesize_core::junk::{Env, Platform};

    use super::*;

    #[test]
    fn rows_tick_what_is_safe_and_open_to_paths() {
        let dir = std::env::temp_dir().join(format!("freesize-junk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let env = Env::under(&dir);
        for path in [
            "~/.cache/thumbnails/large/a.png",
            "~/Downloads/setup.deb",
            "$VAR/cache/apt/archives/x.deb",
        ] {
            let path = env.expand(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"junk").unwrap();
        }
        let mut state = State::default();
        let findings = junk::find(Platform::Linux, &env, &[], junk::now());
        state.found(findings, 100);
        assert_eq!(state.ticked, [false, true, false]);
        assert_eq!(state.chosen_bytes(), 4);
        let rows = state.rows(false);
        let kinds: Vec<i32> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(kinds, [CATEGORY, RULE, RULE, CATEGORY, RULE]);
        assert_eq!(rows[1].title, "Downloaded packages (apt)");
        assert_eq!(rows[1].action, "Show command");
        assert!(!rows[1].enabled && !rows[1].ticked);
        assert!(rows[2].enabled && rows[2].ticked);
        assert!(
            rows[4]
                .subtitle
                .ends_with("Look first; goes to the quarantine.")
        );
        // Unticking, and an unsafe rule can't be ticked.
        state.toggle(1);
        state.toggle(2);
        assert_eq!(state.ticked, [false, false, false]);
        state.toggle(1);
        state.toggle_open(2);
        let rows = state.rows(false);
        assert_eq!(rows[5].kind, ITEM);
        assert_eq!(
            state.item_path(rows[5].id),
            Some(env.expand("~/Downloads/setup.deb"))
        );
        assert!(state.confirm_text().starts_with("Delete 4 B for good?"));
        assert_eq!(state.settings.found, 12);
        assert_eq!(state.settings.safe, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reminders_cycle_and_come_due() {
        let mut settings = Settings::default();
        assert!(!settings.due(1_000_000));
        settings.remind = settings.next_remind().into();
        assert_eq!(settings.remind, "weekly");
        assert!(settings.due(1_000_000));
        settings.checked = 1_000_000;
        assert!(!settings.due(1_000_000 + 6 * 86_400));
        assert!(settings.due(1_000_000 + 8 * 86_400));
        settings.remind = settings.next_remind().into();
        assert_eq!(settings.remind, "daily");
        assert!(settings.due(1_000_000 + 86_400));
        assert_eq!(settings.next_remind(), "off");
        let state = State {
            settings: Settings {
                remind: "weekly".into(),
                checked: 50,
                found: 2_400 * 1024 * 1024,
                safe: 1_000 * 1024 * 1024,
            },
            ..State::default()
        };
        let rows = state.home_rows(100);
        assert_eq!(rows[0].action, "Daily");
        assert_eq!(
            rows[1].title,
            "2.34 GB of junk to review, 1000 MB safe to clean"
        );
        assert_eq!(rows[1].key, OPEN);
        assert_eq!(
            tool_url("Settings → System → Storage → Temporary files"),
            Some("ms-settings:storagesense")
        );
        assert_eq!(tool_url("sudo apt clean"), None);
    }

    #[test]
    fn outcome_summary() {
        let outcome = Outcome {
            freed: 1_000,
            quarantined: 500,
            skipped: vec![junk::Skipped {
                path: PathBuf::from("/x"),
                why: "busy".into(),
            }],
            refills: 400,
            failed: vec![("Chrome's cache".into(), "Chrome is running".into())],
        };
        assert_eq!(
            outcome.summary(),
            "Freed 1000 B. Moved 500 B to the quarantine; it's freed when you empty it. Skipped \
             1 file in use. 400 B of that refills within days of normal use. Chrome's cache: \
             Chrome is running."
        );
    }
}
