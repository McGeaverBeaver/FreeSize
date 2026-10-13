//! The Clean junk screen on desktops: what the known places hold, by
//! category, ticked where it's safe, and the result afterwards. The
//! finding and deleting is `freesize_core::junk`; "check first" items go to
//! this device's quarantine. An optional daily or weekly check says on the
//! home screen when junk piles up; nothing is ever cleaned on its own.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use freesize_agent::junk as cleaner;
use freesize_core::format_size;
use freesize_core::junk::{self, Category, Finding, Rule, Safety};
use serde::{Deserialize, Serialize};

use crate::home::{count, heading, notice};
use crate::{HomeRow, JunkRow, JunkStep};

/// Action ids of the home screen's Clean junk cards.
pub const FIND: &str = "junk-find";
pub const OPEN: &str = "junk-open";
pub const REMIND: &str = "junk-remind";
/// The reminder's settings, in the order Settings offers them.
pub const REMIND_CHOICES: [&str; 3] = ["off", "weekly", "daily"];

/// Row kinds of the Clean junk screen.
const CATEGORY: i32 = 0;
const RULE: i32 = 1;
const ITEM: i32 = 2;
const NOTE: i32 = 3;
/// A rule removed with the system's own tool: no tick box, its buttons.
const TOOL: i32 = 4;
/// Under an opened tool rule: its command, Copy and Run.
const COMMAND: i32 = 5;

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
    /// Stop was pressed: the rules after the one cleaning then were left.
    pub stopped: bool,
    /// What happened per rule, for the results page.
    pub steps: Vec<Step>,
}

impl Outcome {
    pub fn summary(&self) -> String {
        format!("Freed {}.{}", format_size(self.freed), self.details())
    }

    /// The summary without its first sentence, for under the results
    /// page's big line.
    pub fn details(&self) -> String {
        let mut text = String::new();
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
    /// Which page the screen shows.
    pub stage: Stage,
    /// The clean running now, for its progress page.
    pub progress: Option<Arc<Progress>>,
    /// Tool rules whose command is shown, by rule id (indexes change
    /// when a look finds things in another order).
    pub commands_open: HashSet<&'static str>,
    /// What a command printed, per rule id.
    pub command_output: HashMap<&'static str, String>,
    /// The rule whose command runs now.
    pub command_running: Option<&'static str>,
    /// This window runs as administrator (or root), so commands needing
    /// it run as they are.
    pub elevated: bool,
    /// The phone's junk: the categories' desktop blurbs don't fit it.
    pub phone: bool,
}

/// The Clean junk screen's pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage {
    /// The rules, to tick.
    #[default]
    List,
    /// The progress page while cleaning.
    Cleaning,
    /// What cleaning did.
    Done,
}

impl Stage {
    /// The number the screen knows it by.
    pub fn key(self) -> i32 {
        match self {
            Stage::List => 0,
            Stage::Cleaning => 1,
            Stage::Done => 2,
        }
    }
}

/// One rule of a clean, as the progress and results pages show it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub title: String,
    /// What it held when looked at.
    pub bytes: u64,
    pub state: StepState,
    /// "Freed 2.1 GB", "3 files in use stayed", or why it failed.
    pub detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepState {
    Waiting,
    Running,
    Done,
    Failed,
    /// Not started, since Stop was pressed.
    Left,
}

impl StepState {
    fn key(self) -> i32 {
        match self {
            StepState::Waiting => 0,
            StepState::Running => 1,
            StepState::Done => 2,
            StepState::Failed => 3,
            StepState::Left => 4,
        }
    }
}

/// A clean in progress, shared with the thread doing it.
#[derive(Default)]
pub struct Progress {
    /// Stop after the rule cleaning now.
    pub stop: AtomicBool,
    pub state: Mutex<ProgressState>,
}

#[derive(Clone, Debug, Default)]
pub struct ProgressState {
    pub steps: Vec<Step>,
    /// Files gone or set aside so far, and their bytes.
    pub files: u64,
    pub bytes: u64,
    /// What all the chosen rules hold.
    pub total: u64,
}

impl Progress {
    fn update(&self, change: impl FnOnce(&mut ProgressState)) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        change(&mut state);
    }

    pub fn snapshot(&self) -> ProgressState {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
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

    /// Ticks or unticks a rule: safe and "check first" rules can be, when
    /// their program isn't running; rules removed with the system's own
    /// tool can't.
    pub fn toggle(&mut self, index: usize) {
        if let (Some(finding), Some(ticked)) =
            (self.findings.get(index), self.ticked.get_mut(index))
            && finding.can_tick()
        {
            *ticked = !*ticked;
        }
    }

    pub fn toggle_open(&mut self, index: usize) {
        if !self.open.remove(&index) {
            self.open.insert(index);
        }
    }

    /// Shows or hides a tool rule's command.
    pub fn toggle_command(&mut self, index: usize) {
        if let Some(id) = self.rule_id(index)
            && !self.commands_open.remove(id)
        {
            self.commands_open.insert(id);
        }
    }

    /// The id of the rule at `index`.
    pub fn rule_id(&self, index: usize) -> Option<&'static str> {
        self.findings.get(index).map(|f| f.rule.id)
    }

    /// The command of a tool rule, if it has one FreeSize can run.
    pub fn command_of(&self, index: usize) -> Option<&'static Command> {
        self.findings.get(index).and_then(|f| command(f.rule.id))
    }

    /// The question before running a tool rule's command.
    pub fn run_confirm_text(&self, index: usize) -> Option<String> {
        let command = self.command_of(index)?;
        Some(format!(
            "Run “{}”? {} {}",
            command.shown, command.changes, command.undo
        ))
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
        let trash: u64 = chosen
            .iter()
            .filter(|f| f.rule.empties_trash())
            .map(|f| f.bytes)
            .sum();
        let deleted: u64 = chosen
            .iter()
            .filter(|f| f.rule.safety == Safety::Safe && !f.rule.empties_trash())
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
        if trash > 0 {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(&format!(
                "Empty the trash for good ({})? Those are files you deleted earlier; they \
                 can't be put back afterwards.",
                format_size(trash)
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

    /// The rows of the list page.
    pub fn rows(&self, busy: bool) -> Vec<JunkRow> {
        let mut rows = Vec::new();
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
                .filter(|(_, f)| f.rule.category == category && f.rule.safety != Safety::UseTool)
                .collect();
            if in_category.is_empty() {
                continue;
            }
            let total: u64 = in_category.iter().map(|(_, f)| f.bytes).sum();
            rows.push(JunkRow {
                kind: CATEGORY,
                title: category.title().into(),
                subtitle: if self.phone {
                    "".into()
                } else {
                    category.blurb().into()
                },
                detail: format_size(total).into(),
                ..Default::default()
            });
            for (index, finding) in in_category {
                let mut subtitle = finding.rule.blurb.to_string();
                if finding.rule.refills {
                    subtitle.push_str(" Refills within days.");
                }
                // A running program is said on its badge instead; the
                // trash's blurb says already that emptying it is for good.
                if !finding.running && !finding.rule.empties_trash() {
                    let why = finding.why_not();
                    if !why.is_empty() {
                        subtitle.push(' ');
                        subtitle.push_str(&why);
                    }
                }
                rows.push(JunkRow {
                    kind: RULE,
                    id: index as i32,
                    title: finding.rule.title.into(),
                    subtitle: subtitle.into(),
                    detail: self.rule_detail(finding).into(),
                    ticked: self.ticked.get(index).copied().unwrap_or(false),
                    enabled: finding.can_tick(),
                    open: self.open.contains(&index),
                    badge: if finding.running {
                        format!("{} is open", finding.rule.app).into()
                    } else {
                        "".into()
                    },
                    ..Default::default()
                });
                self.item_rows(index, finding, &mut rows);
            }
        }

        // Rules only the system's own tools clean, last and without a box.
        let tools: Vec<(usize, &Finding)> = self
            .findings
            .iter()
            .enumerate()
            .filter(|(_, f)| f.rule.safety == Safety::UseTool)
            .collect();
        if !tools.is_empty() {
            rows.push(JunkRow {
                kind: CATEGORY,
                title: tools_title().into(),
                subtitle: "FreeSize can't clean these itself. Each button opens the right \
                           settings page or shows the command; Run asks first."
                    .into(),
                detail: format_size(tools.iter().map(|(_, f)| f.bytes).sum()).into(),
                ..Default::default()
            });
        }
        for (index, finding) in tools {
            let mut subtitle = finding.rule.blurb.to_string();
            if finding.rule.refills {
                subtitle.push_str(" Refills within days.");
            }
            let command = command(finding.rule.id);
            let id = finding.rule.id;
            let shown = self.commands_open.contains(id);
            rows.push(JunkRow {
                kind: TOOL,
                id: index as i32,
                title: finding.rule.title.into(),
                subtitle: subtitle.into(),
                detail: self.rule_detail(finding).into(),
                open: self.open.contains(&index),
                // The first button: the settings page, when there is one.
                action: if tool_url(finding.rule.tool).is_some() {
                    "Open settings".into()
                } else {
                    "".into()
                },
                // The second: the command, when there is one.
                second: match (
                    command.is_some() || tool_url(finding.rule.tool).is_none(),
                    shown,
                ) {
                    (false, _) => "".into(),
                    (true, false) => "Show command".into(),
                    (true, true) => "Hide command".into(),
                },
                ..Default::default()
            });
            if shown {
                let running = self.command_running == Some(id);
                let (text, about, run) = match command {
                    Some(command) => (
                        command.shown.to_string(),
                        format!("{} {}", command.changes, command.undo),
                        if running {
                            "Running…".to_string()
                        } else if command.admin && !self.elevated {
                            "Run as administrator".to_string()
                        } else {
                            "Run".to_string()
                        },
                    ),
                    // Named only: copied and run by the person.
                    None => (
                        finding.rule.tool.to_string(),
                        "Run it in a terminal.".to_string(),
                        String::new(),
                    ),
                };
                rows.push(JunkRow {
                    kind: COMMAND,
                    id: index as i32,
                    title: text.into(),
                    subtitle: about.into(),
                    detail: self
                        .command_output
                        .get(id)
                        .cloned()
                        .unwrap_or_default()
                        .into(),
                    action: run.into(),
                    enabled: !running && self.command_running.is_none(),
                    ..Default::default()
                });
            }
            self.item_rows(index, finding, &mut rows);
        }
        rows
    }

    fn rule_detail(&self, finding: &Finding) -> String {
        format!(
            "{} · {}",
            format_size(finding.bytes),
            count(finding.items.len(), "item", "items")
        )
    }

    /// The paths of an opened rule.
    fn item_rows(&self, index: usize, finding: &Finding, rows: &mut Vec<JunkRow>) {
        if !self.open.contains(&index) {
            return;
        }
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

    /// The summary card's big line: what Clean would free.
    pub fn headline(&self) -> String {
        if self.findings.is_empty() {
            return String::new();
        }
        match self.chosen_bytes() {
            0 => "Tick what goes".into(),
            bytes => format!("Free {}", format_size(bytes)),
        }
    }

    /// The counts under it.
    pub fn summary(&self) -> String {
        if self.findings.is_empty() {
            return String::new();
        }
        let ticked = self.ticked.iter().filter(|t| **t).count();
        let check = self
            .findings
            .iter()
            .zip(&self.ticked)
            .filter(|(f, t)| !**t && f.rule.safety != Safety::UseTool)
            .count();
        let tools = self
            .findings
            .iter()
            .filter(|f| f.rule.safety == Safety::UseTool)
            .count();
        let mut parts = vec![
            format!("{ticked} ticked"),
            format!("{check} to check first"),
        ];
        if tools > 0 {
            parts.push(format!("{tools} {}", tools_short()));
        }
        format!(
            "{} · {} found in all",
            parts.join(" · "),
            format_size(junk::total(&self.findings))
        )
    }

    /// The progress page: its steps, the bar and the line under it.
    pub fn progress_view(&self) -> (Vec<JunkStep>, f32, String) {
        let Some(progress) = &self.progress else {
            return (Vec::new(), 0.0, String::new());
        };
        let state = progress.snapshot();
        let fraction = if state.total > 0 {
            (state.bytes as f64 / state.total as f64).min(1.0) as f32
        } else {
            0.0
        };
        let line = format!(
            "{} of {} · {}",
            format_size(state.bytes),
            format_size(state.total),
            count(state.files as usize, "file", "files")
        );
        (steps(&state.steps), fraction, line)
    }

    /// The results page: the big line, the lines under it and the steps.
    pub fn results_view(&self) -> (String, String, Vec<JunkStep>) {
        let Some(outcome) = &self.outcome else {
            return (String::new(), String::new(), Vec::new());
        };
        let headline = match (outcome.freed, outcome.quarantined) {
            (0, 0) => "Nothing was freed".to_string(),
            (0, aside) => format!("Moved {} to the quarantine", format_size(aside)),
            (freed, _) => format!("Freed {}", format_size(freed)),
        };
        let mut details = outcome.details().trim().to_string();
        if outcome.stopped {
            details.insert_str(0, "Stopped before the end; what's left is still listed. ");
        }
        (headline, details.trim().to_string(), steps(&outcome.steps))
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

    /// Clean's junk cards.
    pub fn home_rows(&self, now: i64) -> Vec<HomeRow> {
        let mut rows = vec![heading("Clean junk", "", None)];
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
        rows
    }

    /// The reminder, a line in Settings.
    pub fn settings_row(&self) -> HomeRow {
        crate::home::choice(
            "Remind me about junk",
            "FreeSize looks in the known places and says on Clean when junk piles up. It \
             never cleans anything by itself.",
            REMIND,
            &["Off", "Once a week", "Once a day"],
            REMIND_CHOICES
                .iter()
                .position(|c| *c == self.settings.remind)
                .unwrap_or(0),
        )
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

/// The phone's rules that the junk cleaner doesn't have: what the app
/// finds in shared storage (`cleanup::find` and the trash).
static PHONE_RULES: [Rule; 4] = [
    phone_rule(
        "phone-trash",
        Category::Trash,
        "Trash",
        "Photos and videos you deleted in the last 30 days. They take space until Android \
         removes them for good.",
        Safety::CheckFirst,
    ),
    phone_rule(
        "installers",
        Category::Trash,
        "Old app installers",
        "APK files in Download. Installers FreeSize saved for reinstalling apps are kept.",
        Safety::CheckFirst,
    ),
    phone_rule(
        "thumbnails",
        Category::System,
        "Thumbnail cache",
        "Old preview images in DCIM/.thumbnails. Gallery apps make new ones when they need \
         them.",
        Safety::Safe,
    ),
    phone_rule(
        "leftovers",
        Category::Trash,
        "Leftovers of removed apps",
        "Folders in Android/media of apps that are no longer installed.",
        Safety::CheckFirst,
    ),
];

const fn phone_rule(
    id: &'static str,
    category: Category,
    title: &'static str,
    blurb: &'static str,
    safety: Safety,
) -> Rule {
    Rule {
        id,
        category,
        title,
        blurb,
        refills: false,
        safety,
        app: "",
        processes: &[],
        tool: "",
        patterns: &[],
        take: junk::Take::Whole,
        keep: &[],
        min_age_days: 0,
        fixture: junk::Fixture {
            hit: &[],
            miss: &[],
        },
    }
}

/// The phone's junk as the Clean junk screen lists it: one finding per
/// kind, one item per path.
pub fn phone_findings(found: &[crate::cleanup::Junk]) -> Vec<Finding> {
    use crate::cleanup::JunkKind;
    let mut findings: Vec<Finding> = Vec::new();
    for junk in found {
        if junk.paths.is_empty() {
            continue;
        }
        let rule: Option<&'static Rule> = match junk.kind {
            JunkKind::Trash => Some(&PHONE_RULES[0]),
            JunkKind::Installers => Some(&PHONE_RULES[1]),
            JunkKind::Thumbnails => Some(&PHONE_RULES[2]),
            JunkKind::Leftovers => Some(&PHONE_RULES[3]),
            other => junk::rule_by_id(junk::Platform::Android, other.id()),
        };
        let Some(rule) = rule else {
            continue;
        };
        // Sizes per path where they can be read; the kind's total says the rest.
        let mut items: Vec<junk::Item> = junk
            .paths
            .iter()
            .map(|path| junk::Item {
                path: path.clone(),
                bytes: freesize_agent::quarantine::size_on_disk(path),
                files: 1,
                covers: junk::Covers::Itself,
                note: String::new(),
            })
            .collect();
        let measured: u64 = items.iter().map(|i| i.bytes).sum();
        if measured == 0
            && let Some(first) = items.first_mut()
        {
            first.bytes = junk.bytes;
        }
        items.sort_by_key(|i| std::cmp::Reverse(i.bytes));
        findings.push(Finding {
            rule,
            bytes: junk.bytes,
            files: items.len() as u64,
            items,
            running: false,
        });
    }
    findings.sort_by_key(|f| f.rule.category);
    findings
}

/// The section of rules cleaned with the system's own tools.
fn tools_title() -> &'static str {
    if cfg!(windows) {
        "Clean with Windows' own tools"
    } else if cfg!(target_os = "macos") {
        "Clean with macOS's own tools"
    } else if cfg!(target_os = "android") {
        "Clean with Android's own tools"
    } else {
        "Clean with the system's own tools"
    }
}

fn tools_short() -> &'static str {
    if cfg!(windows) {
        "with Windows' tools"
    } else {
        "with the system's tools"
    }
}

fn steps(steps: &[Step]) -> Vec<JunkStep> {
    steps
        .iter()
        .map(|step| JunkStep {
            title: step.title.as_str().into(),
            detail: match step.state {
                StepState::Waiting => format_size(step.bytes),
                StepState::Left => "Not cleaned: stopped before it".to_string(),
                _ => step.detail.clone(),
            }
            .into(),
            state: step.state.key(),
        })
        .collect()
}

/// A command that removes a tool rule's junk, which FreeSize can run when
/// asked (and, where it needs one, with an administrator's rights).
#[derive(Debug, PartialEq, Eq)]
pub struct Command {
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// As shown and copied.
    pub shown: &'static str,
    /// What running it changes.
    pub changes: &'static str,
    /// How to undo it, or that there's nothing to undo.
    pub undo: &'static str,
    pub admin: bool,
}

const COMMANDS: &[(&str, Command)] = &[
    (
        "hibernation",
        Command {
            program: "powercfg",
            args: &["/h", "off"],
            shown: "powercfg /h off",
            changes: "Hibernation and Fast Startup turn off, and Windows deletes hiberfil.sys.",
            undo: "To turn them back on: powercfg /h on, as administrator.",
            admin: true,
        },
    ),
    (
        "delivery-optimization",
        Command {
            program: "powershell",
            args: &[
                "-NoProfile",
                "-Command",
                "Delete-DeliveryOptimizationCache -Force",
            ],
            shown: "Delete-DeliveryOptimizationCache -Force",
            changes: "Windows empties its cache of update files kept for other PCs.",
            undo: "Nothing to undo: Windows downloads update files again when it needs them.",
            admin: true,
        },
    ),
    (
        "ios-simulators",
        Command {
            program: "xcrun",
            args: &["simctl", "delete", "unavailable"],
            shown: "xcrun simctl delete unavailable",
            changes: "Xcode removes simulators whose runtime isn't installed any more.",
            undo: "Add simulators again in Xcode's Devices and Simulators window.",
            admin: false,
        },
    ),
    (
        "journal",
        Command {
            program: "journalctl",
            args: &["--vacuum-size=200M"],
            shown: "sudo journalctl --vacuum-size=200M",
            changes: "systemd keeps the newest 200 MB of logs and deletes the older ones.",
            undo: "Deleted logs can't be brought back.",
            admin: true,
        },
    ),
    (
        "apt-cache",
        Command {
            program: "apt-get",
            args: &["clean"],
            shown: "sudo apt clean",
            changes: "apt deletes the package files of what it already installed.",
            undo: "Nothing to undo: apt downloads a package again if it needs it.",
            admin: true,
        },
    ),
    (
        "dnf-cache",
        Command {
            program: "dnf",
            args: &["clean", "all"],
            shown: "sudo dnf clean all",
            changes: "dnf deletes its downloaded packages and metadata.",
            undo: "Nothing to undo: dnf downloads them again when it needs them.",
            admin: true,
        },
    ),
    (
        "pacman-cache",
        Command {
            program: "paccache",
            args: &["-r"],
            shown: "sudo paccache -r",
            changes: "pacman's cache keeps the three newest versions of each package.",
            undo: "Older versions can't be brought back from the cache.",
            admin: true,
        },
    ),
];

/// The command FreeSize can run for a rule.
pub fn command(rule: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|(id, _)| *id == rule).map(|(_, c)| c)
}

/// Runs a command, waiting for it; what it printed, or why it failed.
/// `elevated`: this process has the rights already. Otherwise macOS and
/// Linux ask for an administrator's password; Windows is relaunched
/// before this is reached.
pub fn run_command(command: &Command, elevated: bool) -> Result<String, String> {
    #[cfg(target_os = "android")]
    {
        let _ = (command, elevated);
        Err("not on a phone".into())
    }
    #[cfg(not(target_os = "android"))]
    {
        let mut process = if command.admin && !elevated && cfg!(target_os = "macos") {
            let line = std::iter::once(command.program)
                .chain(command.args.iter().copied())
                .map(|a| format!("'{}'", a.replace('\'', r"'\''")))
                .collect::<Vec<_>>()
                .join(" ");
            let mut p = std::process::Command::new("osascript");
            p.arg("-e").arg(format!(
                "do shell script \"{}\" with administrator privileges",
                line.replace('\\', "\\\\").replace('"', "\\\"")
            ));
            p
        } else if command.admin && !elevated && cfg!(unix) {
            let mut p = std::process::Command::new("pkexec");
            p.arg(command.program).args(command.args);
            p
        } else {
            let mut p = std::process::Command::new(command.program);
            p.args(command.args);
            p
        };
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            // No console window flashing up.
            process.creation_flags(0x0800_0000);
        }
        let output = process
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|e| format!("couldn't start {}: {e}", command.program))?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        // The last lines say how it went; the rest is progress.
        let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let tail = lines[lines.len().saturating_sub(6)..].join("\n");
        if output.status.success() {
            Ok(if tail.is_empty() {
                "Done.".into()
            } else {
                tail
            })
        } else if tail.is_empty() {
            Err(format!("it stopped with {}", output.status))
        } else {
            Err(tail)
        }
    }
}

/// Cleans the chosen findings: safe ones deleted, "check first" ones into
/// this device's quarantine, as one batch. Slow for big folders, so it
/// runs off the UI thread, saying how far it got in `progress` and
/// stopping between rules when asked to.
pub fn clean(chosen: Vec<Finding>, progress: &Progress) -> Outcome {
    progress.update(|state| {
        state.total = chosen.iter().map(|f| f.bytes).sum();
        state.steps = chosen
            .iter()
            .map(|f| Step {
                title: f.rule.title.to_string(),
                bytes: f.bytes,
                state: StepState::Waiting,
                detail: String::new(),
            })
            .collect();
    });
    let batch = crate::quarantine::new_batch_id();
    let mut outcome = Outcome::default();
    for (index, finding) in chosen.iter().enumerate() {
        if progress.stop.load(Ordering::Relaxed) {
            outcome.stopped = true;
            progress.update(|state| {
                for step in &mut state.steps[index..] {
                    step.state = StepState::Left;
                }
            });
            break;
        }
        progress.update(|state| state.steps[index].state = StepState::Running);
        let title = finding.rule.title;
        let result: Result<cleaner::Outcome, String> = if finding.running {
            Err(format!("{} is open; close it first", finding.rule.app))
        } else {
            match finding.rule.safety {
                Safety::UseTool => Err(format!("use its own tool: {}", finding.rule.tool)),
                Safety::Safe => {
                    let mut done = cleaner::Outcome::default();
                    for item in &finding.items {
                        let cleaned = junk::delete(item);
                        done.freed += cleaned.freed;
                        let gone = item.files.saturating_sub(cleaned.skipped.len() as u64);
                        progress.update(|state| {
                            state.bytes += cleaned.freed;
                            state.files += gone;
                        });
                        done.skipped.extend(cleaned.skipped);
                    }
                    Ok(done)
                }
                Safety::CheckFirst => {
                    let paths: Vec<PathBuf> =
                        finding.items.iter().flat_map(|i| i.paths()).collect();
                    let (bytes, errors) =
                        crate::quarantine::delete_group(&batch, "Clean junk", title, &paths);
                    progress.update(|state| {
                        state.bytes += bytes;
                        state.files += finding.files;
                    });
                    match errors.into_iter().next() {
                        Some(err) if bytes == 0 => Err(err),
                        _ => Ok(cleaner::Outcome {
                            quarantined: bytes,
                            ..Default::default()
                        }),
                    }
                }
            }
        };
        let step = match result {
            Ok(done) => {
                if finding.rule.refills {
                    outcome.refills += done.freed;
                }
                outcome.freed += done.freed;
                outcome.quarantined += done.quarantined;
                let detail = done.detail();
                outcome.skipped.extend(done.skipped);
                (StepState::Done, capitalize(&detail))
            }
            Err(err) => {
                outcome.failed.push((title.to_string(), err.clone()));
                (StepState::Failed, capitalize(&err))
            }
        };
        progress.update(|state| {
            state.steps[index].state = step.0;
            state.steps[index].detail = step.1;
        });
    }
    outcome.steps = progress.snapshot().steps;
    outcome
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use freesize_core::junk::{Env, Platform};

    use super::*;

    #[test]
    fn emptying_the_trash_is_asked_for_by_name() {
        let dir = std::env::temp_dir().join(format!("freesize-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let env = Env::under(&dir);
        for path in [
            "~/.cache/thumbnails/large/a.png",
            "~/.local/share/Trash/files/old.pdf",
        ] {
            let path = env.expand(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"junk").unwrap();
        }
        let mut state = State::default();
        let findings = junk::find(Platform::Linux, &env, &[], junk::now());
        state.found(findings, 100);
        let trash = state
            .findings
            .iter()
            .position(|f| f.rule.id == "trash")
            .unwrap();
        assert!(!state.ticked[trash]);
        assert!(!state.confirm_text().contains("trash"));
        state.toggle(trash);
        let text = state.confirm_text();
        assert!(text.starts_with("Delete 4 B for good? Caches"), "{text}");
        assert!(text.contains("Empty the trash for good (4 B)?"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        // Rules only the system's tools clean come last, without a box.
        assert_eq!(kinds, [CATEGORY, RULE, CATEGORY, RULE, CATEGORY, TOOL]);
        assert_eq!(rows[5].title, "Downloaded packages (apt)");
        assert_eq!(rows[5].second, "Show command");
        assert_eq!(rows[5].action, "");
        assert!(rows[1].enabled && rows[1].ticked);
        assert!(
            rows[3]
                .subtitle
                .ends_with("Look first; goes to the quarantine.")
        );
        // A "check first" rule starts unticked but can be ticked.
        assert!(rows[3].enabled && !rows[3].ticked);
        assert_eq!(
            state.summary(),
            format!(
                "1 ticked · 1 to check first · 1 {} · 12 B found in all",
                tools_short()
            )
        );
        assert_eq!(state.headline(), "Free 4 B");
        // Unticking, ticking a "check first" rule, and a rule removed with
        // the system's own tool can't be ticked.
        state.toggle(1);
        state.toggle(2);
        state.toggle(0);
        assert_eq!(state.ticked, [false, false, true]);
        assert!(
            state
                .confirm_text()
                .starts_with("4 B goes to the quarantine")
        );
        state.toggle(2);
        state.toggle(1);
        state.toggle_open(2);
        let rows = state.rows(false);
        assert_eq!(rows[4].kind, ITEM);
        assert_eq!(
            state.item_path(rows[4].id),
            Some(env.expand("~/Downloads/setup.deb"))
        );
        // The command opens under its rule, ready to copy and run.
        state.toggle_command(0);
        state.elevated = false;
        let rows = state.rows(false);
        let command = rows.iter().find(|r| r.kind == COMMAND).unwrap();
        assert_eq!(command.title, "sudo apt clean");
        assert_eq!(command.action, "Run as administrator");
        assert!(
            state
                .run_confirm_text(0)
                .unwrap()
                .starts_with("Run “sudo apt clean”? apt deletes")
        );
        assert!(state.confirm_text().starts_with("Delete 4 B for good?"));
        assert_eq!(state.settings.found, 12);
        assert_eq!(state.settings.safe, 4);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn cleaning_says_how_far_it_got_and_stops_between_rules() {
        let dir = std::env::temp_dir().join(format!("freesize-junk-clean-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let env = Env::under(&dir);
        for path in ["~/.cache/thumbnails/large/a.png", "~/.cache/pip/http/x"] {
            let path = env.expand(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"junk").unwrap();
        }
        let findings: Vec<Finding> = junk::find(Platform::Linux, &env, &[], junk::now())
            .into_iter()
            .filter(|f| f.rule.safety == Safety::Safe)
            .collect();
        assert_eq!(findings.len(), 2);
        let progress = Progress::default();
        let outcome = clean(findings.clone(), &progress);
        assert_eq!(outcome.freed, 8);
        assert!(outcome.steps.iter().all(|s| s.state == StepState::Done));
        assert_eq!(outcome.steps[0].detail, "Freed 4 B");
        let state = progress.snapshot();
        assert_eq!((state.bytes, state.total, state.files), (8, 8, 2));

        // Stop before it starts: nothing goes, every rule is left out.
        let progress = Progress::default();
        progress.stop.store(true, Ordering::Relaxed);
        let outcome = clean(findings, &progress);
        assert!(outcome.stopped);
        assert!(outcome.steps.iter().all(|s| s.state == StepState::Left));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reminders_come_due() {
        let mut settings = Settings::default();
        assert!(!settings.due(1_000_000));
        settings.remind = "weekly".into();
        assert!(settings.due(1_000_000));
        settings.checked = 1_000_000;
        assert!(!settings.due(1_000_000 + 6 * 86_400));
        assert!(settings.due(1_000_000 + 8 * 86_400));
        settings.remind = "daily".into();
        assert!(settings.due(1_000_000 + 86_400));
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
        assert_eq!(rows[0].action, "");
        // Settings shows the reminder as it is now, not what a tap would make it.
        assert_eq!(state.settings_row().chosen, 1);
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
            ..Outcome::default()
        };
        assert_eq!(
            outcome.summary(),
            "Freed 1000 B. Moved 500 B to the quarantine; it's freed when you empty it. Skipped \
             1 file in use. 400 B of that refills within days of normal use. Chrome's cache: \
             Chrome is running."
        );
    }
}
