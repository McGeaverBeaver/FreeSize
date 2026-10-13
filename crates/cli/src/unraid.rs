//! `disktree unraid`: the Unraid plugin's background work, and the commands
//! its page in the web UI uses.
//!
//! The daemon scans the disks that are awake on a schedule, watches what
//! keeps disks spun up, runs moves between disks, writes what it found to
//! `status.json` for the page, and, once paired, reports to the Hub and
//! carries out what the server's approver allows. The page asks it for
//! things by dropping files in `requests/`; quick actions (the quarantine,
//! Docker cleanup, plans) the page runs itself with the commands below.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};
use disktree_agent::commands::{Deleted, Executor, Guards, Policy, Request};
use disktree_agent::guard::OpenFiles;
use disktree_agent::quarantine::{Quarantine, QuarantineExecutor};
use disktree_agent::{AgentError, AgentState, Client, UreqTransport, now};
use disktree_core::format_size;
use disktree_proto::unraid::{MoveJob, QuarantineBatch, UnraidReport};
use disktree_proto::{ActionKind, Command, DeviceInfo, Platform, PollResponse, Report, ScanReport};
use disktree_unraid::activity::{self, Monitor};
use disktree_unraid::array::{self, Array};
use disktree_unraid::docker::{self, Engine, Inventory};
use disktree_unraid::protect::{self, Rule, UnraidGuard, UnraidPlaces};
use disktree_unraid::round::{self, Round, Settings};
use disktree_unraid::shares::DiskTops;
use disktree_unraid::{System, ini, mover};
use serde::{Deserialize, Serialize};

use crate::agent::{Outcome, handle, log};

/// Where the plugin keeps its settings and the Hub pairing (flash drive).
const PLUGIN_CONFIG: &str = "/boot/config/plugins/disktree";
/// Where the plugin's page and background part talk (in memory).
const PLUGIN_DATA: &str = "/var/local/disktree";

/// The plugin's Hub pairing, so `disktree agent` and `disktree approver`
/// on Unraid use the same file as the page without `--state`.
pub fn plugin_state() -> PathBuf {
    Path::new(PLUGIN_CONFIG).join("agent.json")
}

/// After pairing from the terminal: asks the background part to tell the
/// Hub everything now, as pairing on the page does. False if it can't.
pub fn tell_daemon_paired() -> bool {
    Dirs::new(DirArgs {
        config: PLUGIN_CONFIG.into(),
        data: PLUGIN_DATA.into(),
        root: "/".into(),
    })
    .request("hub", "")
    .is_ok()
}

#[derive(Args, Debug, Clone)]
pub struct DirArgs {
    /// Settings, the Hub pairing and saved results (on the flash drive).
    #[arg(long, global = true, default_value = PLUGIN_CONFIG)]
    config: PathBuf,
    /// The page's status and requests (in memory).
    #[arg(long, global = true, default_value = PLUGIN_DATA)]
    data: PathBuf,
    /// Where the server's files are; for tests.
    #[arg(long, global = true, default_value = "/", hide = true)]
    root: PathBuf,
}

#[derive(Subcommand, Debug)]
pub enum UnraidAction {
    /// Run in the background: scan, watch the disks, run moves, report to
    /// the Hub and carry out what the approver allows.
    Daemon,
    /// What the background part found last.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Ask the background part to scan now.
    Scan,
    /// What deleting some paths would really free.
    Estimate {
        paths: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Move things aside, put them back, or empty the quarantine.
    Quarantine {
        #[command(subcommand)]
        action: QuarantineAction,
    },
    /// Plan, start or stop moving a share's folder between disks.
    Move {
        #[command(subcommand)]
        action: MoveAction,
    },
    /// The folders in a share (or a folder in it), and which disks hold
    /// each, for choosing what to move.
    Folders {
        #[arg(long)]
        share: String,
        #[arg(long, default_value = "")]
        folder: String,
        /// Look at sleeping disks too, spinning them up.
        #[arg(long)]
        wake: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove unused Docker images or the build cache, by the ids the
    /// status lists.
    DockerCleanup { items: Vec<String> },
}

#[derive(Subcommand, Debug)]
pub enum QuarantineAction {
    List {
        #[arg(long)]
        json: bool,
    },
    /// Move paths aside, on the disks they're on.
    Add {
        paths: Vec<String>,
        #[arg(long, default_value = "")]
        reason: String,
        #[arg(long, default_value = "Unraid web UI")]
        by: String,
    },
    /// Put a batch back where it was.
    Restore { batch: String },
    /// Delete batches for good.
    Empty { batches: Vec<String> },
}

#[derive(Args, Debug, Clone, Serialize, Deserialize)]
pub struct MoveArgs {
    #[arg(long)]
    share: String,
    /// A folder in the share; empty for all of the share on that disk.
    #[arg(long, default_value = "")]
    folder: String,
    /// The disks to move from, one move each; they run one after another.
    #[arg(long, required = true)]
    from: Vec<String>,
    #[arg(long)]
    to: String,
}

#[derive(Subcommand, Debug)]
pub enum MoveAction {
    /// What a move would do, and whether it can.
    Plan {
        #[command(flatten)]
        args: MoveArgs,
        #[arg(long)]
        json: bool,
    },
    /// Hand a move to the background part.
    Start {
        #[command(flatten)]
        args: MoveArgs,
    },
    /// Stop a move between files.
    Cancel { id: String },
}

struct Dirs {
    config: PathBuf,
    data: PathBuf,
    system: System,
}

impl Dirs {
    fn new(args: DirArgs) -> Dirs {
        Dirs {
            config: args.config,
            data: args.data,
            system: System::at(args.root),
        }
    }

    fn state(&self) -> PathBuf {
        self.config.join("agent.json")
    }

    fn requests(&self) -> PathBuf {
        self.data.join("requests")
    }

    /// Asks the daemon for something.
    fn request(&self, name: &str, body: &str) -> Result<(), String> {
        std::fs::create_dir_all(self.requests()).map_err(|e| e.to_string())?;
        write_atomic(&self.requests().join(name), body.as_bytes())
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, bytes)
        .and_then(|()| std::fs::rename(&temporary, path))
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// The plugin's settings, from `disktree.cfg`.
#[derive(Clone, Debug, Serialize)]
struct Config {
    scan_hours: u64,
    wake_days: u32,
    permissions: bool,
    report: bool,
}

fn read_config(dirs: &Dirs) -> Config {
    let cfg = std::fs::read_to_string(dirs.config.join("disktree.cfg"))
        .map(|t| ini::parse_cfg(&t))
        .unwrap_or_default();
    let number = |k: &str, default: u64| cfg.get(k).and_then(|v| v.parse().ok()).unwrap_or(default);
    let yes = |k: &str, default: bool| cfg.get(k).map_or(default, |v| v == "yes");
    Config {
        scan_hours: number("SCAN_HOURS", 24).clamp(1, 24 * 30),
        wake_days: number("WAKE_DAYS", 7).min(365) as u32,
        permissions: yes("PERMISSIONS", true),
        report: yes("REPORT", true),
    }
}

/// What the page shows, written by the daemon.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Status {
    version: String,
    updated: i64,
    /// What the daemon is doing.
    doing: String,
    last_round: Option<i64>,
    next_round: Option<i64>,
    skipped: Option<String>,
    errors: Vec<String>,
    hub: HubStatus,
    report: UnraidReport,
    quarantine: Vec<QuarantineBatch>,
    settings: serde_json::Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct HubStatus {
    paired: bool,
    name: String,
    url: String,
    /// This server's key, to compare in the dashboard.
    device_key: String,
    approver: Option<String>,
    approver_key: Option<String>,
    /// How the connection is going.
    connection: String,
}

pub fn main(action: UnraidAction, args: DirArgs) -> Result<(), String> {
    let dirs = Dirs::new(args);
    match action {
        UnraidAction::Daemon => daemon(&dirs),
        UnraidAction::Status { json } => status(&dirs, json),
        UnraidAction::Scan => {
            dirs.request("scan", "")?;
            println!("Asked for a scan; the status shows when it's done.");
            Ok(())
        }
        UnraidAction::Estimate { paths, json } => estimate(&dirs, &paths, json),
        UnraidAction::Quarantine { action } => quarantine(&dirs, action),
        UnraidAction::Move { action } => moves(&dirs, action),
        UnraidAction::Folders {
            share,
            folder,
            wake,
            json,
        } => folders(&dirs, &share, &folder, wake, json),
        UnraidAction::DockerCleanup { items } => docker_cleanup(&dirs, &items),
    }
}

/// Paths as given on the server, each as the real paths of its copies.
fn real_places(system: &System, array: &Array, path: &str) -> Vec<PathBuf> {
    array
        .physical(path)
        .into_iter()
        .map(|(_, physical)| system.file(&physical))
        .filter(|p| std::fs::symlink_metadata(p).is_ok())
        .collect()
}

/// The paths the owner added in Settings (`keep.txt` on the flash drive).
fn own_rules(dirs: &Dirs) -> Vec<Rule> {
    std::fs::read_to_string(dirs.config.join("keep.txt"))
        .map(|text| protect::own_rules(&text))
        .unwrap_or_default()
}

/// What must never be deleted, worked out now (without scanning), with
/// the owner's own list.
fn current_rules(dirs: &Dirs, array: &Array) -> (Vec<Rule>, Option<Inventory>) {
    let (mut rules, inventory) = protect::current(&dirs.system, array);
    rules.extend(own_rules(dirs));
    (rules, inventory)
}

/// Why a path given on the page can't go to the quarantine, if it can't.
fn refuse(system: &System, rules: &[Rule], path: &str, places: &[PathBuf]) -> Option<String> {
    if !path.starts_with("/mnt/") || path.contains("/../") || path.ends_with("/..") {
        return Some("only files on the array or a pool".into());
    }
    if let Some(why) = disktree_agent::commands::protected_text(path, false, false) {
        return Some(why.into());
    }
    if let Some(why) = protect::check(rules, path) {
        return Some(why);
    }
    if places.is_empty() {
        return Some("it isn't there (or only on a sleeping disk)".into());
    }
    let open = OpenFiles {
        proc: system.file("/proc"),
    };
    use disktree_agent::commands::Guard;
    places.iter().find_map(|p| open.check(p)).or_else(|| {
        let user = system.file(path);
        open.check(&user)
    })
}

fn quarantine_for(system: &System, array: &Array) -> Quarantine {
    Quarantine::new(Box::new(UnraidPlaces {
        system: system.clone(),
        array: array.clone(),
    }))
}

fn quarantine(dirs: &Dirs, action: QuarantineAction) -> Result<(), String> {
    let system = &dirs.system;
    let array = array::read(system);
    let q = quarantine_for(system, &array);
    let changed = match action {
        QuarantineAction::List { json } => {
            let list = q.list();
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&list).unwrap_or_default()
                );
            } else if list.is_empty() {
                println!("The quarantine is empty (on the disks that are awake).");
            } else {
                for batch in list {
                    let items = batch.items.len();
                    println!(
                        "{}  {} {}, {}, by {}{}",
                        batch.id,
                        items,
                        if items == 1 { "item" } else { "items" },
                        format_size(batch.bytes),
                        batch.by,
                        if batch.reason.is_empty() {
                            String::new()
                        } else {
                            format!(" ({})", batch.reason)
                        }
                    );
                    for item in &batch.items {
                        println!("    {}  {}", item.original, format_size(item.bytes));
                    }
                }
            }
            false
        }
        QuarantineAction::Add { paths, reason, by } => {
            let (rules, _) = current_rules(dirs, &array);
            let mut results = Vec::new();
            let mut allowed = Vec::new();
            for path in &paths {
                let path = path.trim_end_matches('/');
                let places = real_places(system, &array, path);
                match refuse(system, &rules, path, &places) {
                    Some(why) => results.push(serde_json::json!({"path": path, "error": why})),
                    None => allowed.push((path.to_string(), system.file(path))),
                }
            }
            if !allowed.is_empty() {
                let id = format!("w{}", now());
                let items: Vec<PathBuf> = allowed.iter().map(|(_, real)| real.clone()).collect();
                let moved = q.move_in(&id, &by, &reason, &items, now());
                for ((path, _), result) in allowed.iter().zip(moved.results) {
                    results.push(match result {
                        Ok(bytes) => serde_json::json!({"path": path, "batch": id, "bytes": bytes}),
                        Err(err) => serde_json::json!({"path": path, "error": err}),
                    });
                }
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&results).unwrap_or_default()
            );
            true
        }
        QuarantineAction::Restore { batch } => {
            println!("{}", q.restore(&batch)?);
            true
        }
        QuarantineAction::Empty { batches } => {
            let freed = q.empty(&batches)?;
            println!("Freed {}.", format_size(freed));
            true
        }
    };
    if changed {
        let _ = dirs.request("refresh", "");
    }
    Ok(())
}

fn estimate(dirs: &Dirs, paths: &[String], json: bool) -> Result<(), String> {
    let system = &dirs.system;
    let array = array::read(system);
    let real: Vec<PathBuf> = paths
        .iter()
        .flat_map(|p| real_places(system, &array, p.trim_end_matches('/')))
        .collect();
    let estimate = disktree_agent::estimate::estimate(&real);
    if json {
        println!(
            "{}",
            serde_json::json!({
                "bytes": estimate.bytes,
                "freed": estimate.freed,
                "linked": estimate.linked,
                "snapshotted": estimate.snapshotted,
                "text": estimate.describe(),
            })
        );
    } else {
        println!("{}", estimate.describe());
    }
    Ok(())
}

fn moves(dirs: &Dirs, action: MoveAction) -> Result<(), String> {
    match action {
        MoveAction::Plan { args, json } => {
            let plans = make_plans(dirs, &args);
            let problems = shared_problems(&plans);
            let ok = problems.is_empty() && plans.iter().all(|p| p.ok());
            if json {
                let plans: Vec<_> = plans
                    .iter()
                    .map(|plan| {
                        serde_json::json!({
                            "ok": plan.ok(),
                            "problems": plan.problems,
                            "warnings": plan.warnings,
                            "job": plan.job,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::json!({ "ok": ok, "problems": problems, "plans": plans })
                );
            } else {
                for plan in &plans {
                    println!(
                        "From {}: {} file{}, {}: {}",
                        plan.job.from,
                        plan.job.files,
                        if plan.job.files == 1 { "" } else { "s" },
                        format_size(plan.job.bytes),
                        if plan.ok() {
                            "can be moved"
                        } else {
                            "can't be moved"
                        }
                    );
                    for line in plan.problems.iter().chain(&plan.warnings) {
                        println!("  {line}");
                    }
                }
                for line in &problems {
                    println!("{line}");
                }
            }
            Ok(())
        }
        MoveAction::Start { args } => {
            let plans = make_plans(dirs, &args);
            let mut problems = shared_problems(&plans);
            for plan in &plans {
                problems.extend(plan.problems.iter().map(|p| match plans.len() {
                    1 => p.clone(),
                    _ => format!("from {}: {p}", plan.job.from),
                }));
            }
            if !problems.is_empty() {
                return Err(problems.join("; "));
            }
            let id = format!("m{}", now());
            dirs.request(
                &format!("move-{id}.json"),
                &serde_json::to_string(&args).unwrap_or_default(),
            )?;
            println!("{id}");
            Ok(())
        }
        MoveAction::Cancel { id } => {
            if !id.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err("no such move".into());
            }
            dirs.request(&format!("cancel-{id}"), "")
        }
    }
}

/// The disks to move from, each once.
fn sources(args: &MoveArgs) -> Vec<String> {
    let mut froms: Vec<String> = Vec::new();
    for from in &args.from {
        if !froms.contains(from) {
            froms.push(from.clone());
        }
    }
    froms
}

/// A move's id: with several disks to move from, one each.
fn move_id(base: &str, index: usize, count: usize) -> String {
    match count {
        1 => base.to_string(),
        _ => format!("{base}s{}", index + 1),
    }
}

/// A plan per disk to move from.
fn make_plans(dirs: &Dirs, args: &MoveArgs) -> Vec<mover::Plan> {
    let system = &dirs.system;
    let array = array::read(system);
    let (rules, _) = current_rules(dirs, &array);
    let now = now();
    let froms = sources(args);
    froms
        .iter()
        .enumerate()
        .map(|(i, from)| {
            let mut plan = mover::plan(
                system,
                &array,
                &rules,
                &args.share,
                &args.folder,
                from,
                &args.to,
                now,
            );
            plan.job.id = move_id(&plan.job.id, i, froms.len());
            plan
        })
        .collect()
}

/// What stops moving from several disks, beyond what stops each move.
fn shared_problems(plans: &[mover::Plan]) -> Vec<String> {
    let (shown, total) = mover::duplicates(plans);
    if total == 0 {
        return Vec::new();
    }
    vec![format!(
        "{} on more than one of these disks, and the share shows only one of each: {}{}. Move \
         from one disk at a time, and keep the copy you want",
        match total {
            1 => "1 file is".to_string(),
            n => format!("{n} files are"),
        },
        shown.join(", "),
        if total > shown.len() { ", …" } else { "" }
    )]
}

fn folders(dirs: &Dirs, share: &str, folder: &str, wake: bool, json: bool) -> Result<(), String> {
    let array = array::read(&dirs.system);
    let found = mover::folders(&dirs.system, &array, share, folder, wake)?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&found).map_err(|e| e.to_string())?
        );
        return Ok(());
    }
    for child in &found.children {
        println!("{}  ({})", child.name, child.disks.join(", "));
    }
    if found.more > 0 {
        println!("… and {} more", found.more);
    }
    if !found.asleep.is_empty() {
        println!(
            "Not looked at, asleep: {} (--wake spins them up)",
            found.asleep.join(", ")
        );
    }
    Ok(())
}

fn docker_cleanup(dirs: &Dirs, items: &[String]) -> Result<(), String> {
    let engine = Engine::new(&dirs.system);
    let inventory = docker::inventory(&engine, &dirs.system)?;
    let freed = docker::cleanup(&engine, &inventory, items)?;
    println!("Freed {}.", format_size(freed));
    let _ = dirs.request("refresh", "");
    Ok(())
}

fn status(dirs: &Dirs, json: bool) -> Result<(), String> {
    let text = std::fs::read_to_string(dirs.data.join("status.json")).map_err(|_| {
        "the background part hasn't written a status yet; is it running?".to_string()
    })?;
    if json {
        println!("{text}");
        return Ok(());
    }
    let status: Status = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    println!(
        "DiskTree {} on Unraid {}",
        status.version, status.report.version
    );
    println!("Now: {}", status.doing);
    if let Some(why) = &status.skipped {
        println!("Not scanning: {why}");
    }
    for disk in &status.report.disks {
        println!(
            "  {:<8} {:>10} free of {:>10}{}",
            disk.name,
            format_size(disk.free),
            format_size(disk.size),
            if disk.asleep { "  (asleep)" } else { "" }
        );
    }
    println!(
        "Quarantine: {} batches, {}",
        status.quarantine.len(),
        format_size(status.quarantine.iter().map(|b| b.bytes).sum())
    );
    println!(
        "Hub: {}",
        if !status.hub.paired {
            "not paired".to_string()
        } else if status.hub.connection.is_empty() {
            format!("{} (waiting to hear back)", status.hub.name)
        } else {
            format!("{} ({})", status.hub.name, status.hub.connection)
        }
    );
    Ok(())
}

// ----- The daemon --------------------------------------------------------

enum Event {
    Progress(String),
    Round(Box<Round>),
    Polled(Result<PollResponse, AgentError>),
    Move(MoveJob, bool),
}

/// Carries out the Hub's requests on Unraid.
struct UnraidExecutor {
    inner: QuarantineExecutor,
    engine: Engine,
    inventory: Option<Inventory>,
}

impl Executor for UnraidExecutor {
    fn delete(&self, path: &Path) -> Result<u64, String> {
        self.inner.delete(path)
    }

    fn delete_all(&self, targets: &[PathBuf], command: &Command) -> Deleted {
        self.inner.delete_all(targets, command)
    }

    fn restore(&self, batch: &str) -> Result<String, String> {
        self.inner.restore(batch)
    }

    fn empty_quarantine(&self, batches: &[String]) -> Result<u64, String> {
        self.inner.empty_quarantine(batches)
    }

    fn docker_cleanup(&self, items: &[String]) -> Result<u64, String> {
        let inventory = self.inventory.as_ref().ok_or("Docker isn't running")?;
        docker::cleanup(&self.engine, inventory, items)
    }

    fn note(&self, request: &Request) -> String {
        self.inner.note(request)
    }
}

struct Daemon {
    dirs: Dirs,
    config: Config,
    tops: BTreeMap<String, DiskTops>,
    round: Option<Box<Round>>,
    round_running: bool,
    next_round: Instant,
    monitor: Monitor,
    open: BTreeMap<String, Vec<disktree_proto::unraid::OpenFile>>,
    moves: Vec<MoveJob>,
    cancels: HashMap<String, Arc<AtomicBool>>,
    doing: String,
    connection: String,
    pending: HashMap<String, Request>,
    events: mpsc::Sender<Event>,
}

impl Daemon {
    fn tops_path(&self) -> PathBuf {
        self.dirs.config.join("disks.json")
    }

    fn moves_path(&self) -> PathBuf {
        self.dirs.data.join("moves.json")
    }

    fn array(&self) -> Array {
        array::read(&self.dirs.system)
    }

    fn quarantine(&self) -> Quarantine {
        quarantine_for(&self.dirs.system, &self.array())
    }

    fn start_round(&mut self) {
        if self.round_running {
            return;
        }
        self.round_running = true;
        let system = self.dirs.system.clone();
        let tops = self.tops.clone();
        let settings = Settings {
            wake_days: self.config.wake_days,
            permissions: self.config.permissions,
        };
        let events = self.events.clone();
        let spawned = std::thread::Builder::new()
            .name("disktree-round".into())
            .spawn(move || {
                let progress = |what: &str| {
                    let _ = events.send(Event::Progress(what.to_string()));
                };
                let found = round::round(&system, &settings, &tops, now(), &progress);
                let _ = events.send(Event::Round(Box::new(found)));
            });
        if spawned.is_err() {
            self.round_running = false;
        }
    }

    fn unraid_report(&self) -> UnraidReport {
        let mut report = self
            .round
            .as_ref()
            .map(|r| r.report.clone())
            .unwrap_or_default();
        report.activity = self.monitor.summary(&self.open);
        report.moves = self.moves.clone();
        report
    }

    /// The policy for a request arriving now, with the rules worked out
    /// now: a container installed since the last round protects its
    /// appdata at once, not at the next round. Also what Docker has now,
    /// for removing images.
    fn policy(&self, array: &Array) -> (Policy, Option<Inventory>) {
        let system = &self.dirs.system;
        let mut roots: Vec<PathBuf> = array
            .file_disks()
            .iter()
            .map(|(_, mount)| system.file(mount))
            .collect();
        roots.push(system.file("/mnt/user"));
        let (rules, inventory) = current_rules(&self.dirs, array);
        let policy = Policy {
            attended: false,
            actions: vec![
                ActionKind::Delete,
                ActionKind::Restore,
                ActionKind::Empty,
                ActionKind::Docker,
            ],
            roots,
            keep: vec![self.dirs.state(), self.dirs.config.clone()],
            guards: Guards(vec![
                Arc::new(OpenFiles {
                    proc: system.file("/proc"),
                }),
                Arc::new(UnraidGuard {
                    system: system.clone(),
                    rules,
                }),
            ]),
        };
        (policy, inventory)
    }

    /// Sends a report to the Hub, if paired and reporting is on.
    fn report(&mut self, report: Report) {
        if !self.config.report {
            return;
        }
        let Ok(Some(state)) = AgentState::load(&self.dirs.state()) else {
            return;
        };
        let transport = UreqTransport::new();
        if let Err(err) = Client::new(&state, &transport).report(&report) {
            self.connection = format!("couldn't report: {err}");
        }
    }

    fn report_round(&mut self) {
        let Ok(Some(state)) = AgentState::load(&self.dirs.state()) else {
            return;
        };
        let Some(found) = &self.round else { return };
        let array = found.array.clone();
        let scans: Vec<ScanReport> = found.scans.clone();
        self.report(Report {
            info: Some(device_info(&self.dirs.system, &state.name, Some(&state))),
            disks: Some(round::hub_disks(&array)),
            quarantine: Some(self.quarantine().list()),
            ..Report::default()
        });
        for scan in scans {
            self.report(Report {
                scan: Some(scan),
                ..Report::default()
            });
        }
        let unraid = self.unraid_report();
        self.report(Report {
            unraid: Some(unraid),
            ..Report::default()
        });
    }

    fn handle_poll(&mut self, response: PollResponse) {
        let path = self.dirs.state();
        let Ok(Some(mut state)) = AgentState::load(&path) else {
            return;
        };
        let array = self.array();
        let (policy, inventory) = self.policy(&array);
        let executor = UnraidExecutor {
            inner: QuarantineExecutor {
                quarantine: quarantine_for(&self.dirs.system, &array),
                device: state.name.clone(),
            },
            engine: Engine::new(&self.dirs.system),
            // What Docker has now; the last round's if it isn't answering.
            inventory: inventory.or_else(|| self.round.as_ref().and_then(|r| r.inventory.clone())),
        };
        let transport = UreqTransport::new();
        let Outcome { changed, ran } = handle(
            &mut state,
            &transport,
            &policy,
            &executor,
            &mut self.pending,
            response,
        );
        if changed && let Err(err) = state.save(&path) {
            log(&format!("couldn't save the pairing: {err}"));
        }
        if ran {
            self.report(Report {
                quarantine: Some(self.quarantine().list()),
                ..Report::default()
            });
        }
    }

    /// Queues a move from each disk the page asked for.
    fn queue_moves(&mut self, id: &str, args: MoveArgs) {
        let froms = sources(&args);
        for (i, from) in froms.iter().enumerate() {
            let mut job_id = move_id(id, i, froms.len());
            // Two moves asked for in the same second.
            while self.moves.iter().any(|m| m.id == job_id) {
                job_id.push('x');
            }
            self.moves.push(MoveJob {
                id: job_id,
                share: args.share.clone(),
                folder: args.folder.clone(),
                from: from.clone(),
                to: args.to.clone(),
                state: "queued".into(),
                started: now(),
                ..MoveJob::default()
            });
        }
        self.next_move();
        self.save_moves();
        self.write_status();
    }

    /// Starts the next queued move, unless one is running: one at a time
    /// is easier on parity. Each is planned again first, since the one
    /// before may have changed what fits.
    fn next_move(&mut self) {
        if !self.cancels.is_empty() {
            return;
        }
        while let Some(index) = self.moves.iter().position(|m| m.state == "queued") {
            let job = self.moves[index].clone();
            let args = MoveArgs {
                share: job.share.clone(),
                folder: job.folder.clone(),
                from: vec![job.from.clone()],
                to: job.to.clone(),
            };
            let Some(mut plan) = make_plans(&self.dirs, &args).pop() else {
                return;
            };
            plan.job.id = job.id.clone();
            if !plan.ok() {
                let failed = &mut self.moves[index];
                failed.state = "failed".into();
                failed.detail = plan.problems.join("; ");
                failed.finished = Some(now());
                continue;
            }
            self.moves[index] = plan.job.clone();
            let system = self.dirs.system.clone();
            let cancel = Arc::new(AtomicBool::new(false));
            self.cancels.insert(plan.job.id.clone(), cancel.clone());
            let events = self.events.clone();
            let array = self.array();
            let _ = std::thread::Builder::new()
                .name("disktree-move".into())
                .spawn(move || {
                    let quarantine = quarantine_for(&system, &array);
                    let mut progress = |job: &MoveJob| {
                        let _ = events.send(Event::Move(job.clone(), false));
                    };
                    let done = mover::run(&system, &plan, &quarantine, &mut progress, &cancel);
                    let _ = events.send(Event::Move(done, true));
                });
            return;
        }
    }

    /// Keeps the last 20 moves, and every one still to run.
    fn save_moves(&mut self) {
        while self.moves.len() > 20 {
            match self
                .moves
                .iter()
                .position(|m| m.state != "queued" && !self.cancels.contains_key(&m.id))
            {
                Some(oldest) => {
                    self.moves.remove(oldest);
                }
                None => break,
            }
        }
        let _ = write_atomic(
            &self.moves_path(),
            &serde_json::to_vec(&self.moves).unwrap_or_default(),
        );
    }

    /// Requests the page left in `requests/`.
    fn take_requests(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.dirs.requests()) else {
            return;
        };
        let mut refresh = false;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                continue;
            }
            let body = std::fs::read_to_string(entry.path()).unwrap_or_default();
            let _ = std::fs::remove_file(entry.path());
            if name == "scan" {
                self.start_round();
            } else if name == "refresh" {
                refresh = true;
            } else if name == "settings" {
                // Saved on the page: the next round follows the new
                // schedule, counted from the last one.
                self.config = read_config(&self.dirs);
                let every = self.config.scan_hours * 3600;
                let since = self
                    .round
                    .as_ref()
                    .map_or(0, |r| (now() - r.taken).max(0) as u64);
                self.next_round =
                    Instant::now() + Duration::from_secs(every.saturating_sub(since).max(60));
                self.write_status();
            } else if name == "hub" {
                // Just paired: tell the Hub everything known so far.
                self.connection.clear();
                self.report_round();
                self.write_status();
            } else if let Some(id) = name.strip_prefix("cancel-") {
                if let Some(flag) = self.cancels.get(id) {
                    flag.store(true, Ordering::Relaxed);
                } else if let Some(job) = self
                    .moves
                    .iter_mut()
                    .find(|m| m.id == id && m.state == "queued")
                {
                    job.state = "cancelled".into();
                    job.detail = "not started".into();
                    job.finished = Some(now());
                    self.save_moves();
                    self.write_status();
                }
            } else if let Some(id) = name
                .strip_prefix("move-")
                .and_then(|n| n.strip_suffix(".json"))
            {
                match serde_json::from_str::<MoveArgs>(&body) {
                    Ok(args) if id.chars().all(|c| c.is_ascii_alphanumeric()) => {
                        self.queue_moves(id, args)
                    }
                    Ok(_) => log("a move request had a bad id"),
                    Err(err) => log(&format!("a move request was garbled: {err}")),
                }
            }
        }
        if refresh {
            self.report(Report {
                quarantine: Some(self.quarantine().list()),
                ..Report::default()
            });
            self.write_status();
        }
    }

    fn sample(&mut self) {
        let array = self.array();
        let stats = activity::diskstats(&self.dirs.system);
        self.monitor.record(&array, &stats, now());
        let containers: BTreeMap<String, String> = self
            .round
            .as_ref()
            .and_then(|r| r.inventory.as_ref())
            .map(|inv| {
                inv.containers
                    .iter()
                    .map(|c| (c.id.clone(), c.name.clone()))
                    .collect()
            })
            .unwrap_or_default();
        self.open = activity::open_by_disk(&self.dirs.system, &array, &containers);
    }

    fn write_status(&self) {
        let hub = match AgentState::load(&self.dirs.state()) {
            Ok(Some(state)) => HubStatus {
                paired: true,
                name: state.hub_name.clone(),
                url: state.hub.clone(),
                device_key: state.fingerprint(),
                approver: state.approver.as_ref().map(|a| a.name.clone()),
                approver_key: state.approver.as_ref().map(|a| a.key.fingerprint()),
                connection: self.connection.clone(),
            },
            _ => HubStatus::default(),
        };
        let mut report = self.unraid_report();
        // Disks as Unraid sees them now, with this round's scan times.
        let array = self.array();
        let scanned: BTreeMap<&str, i64> = self
            .tops
            .iter()
            .map(|(k, t)| (k.as_str(), t.scanned))
            .collect();
        report.disks = array
            .disks
            .iter()
            .map(|d| {
                let mut disk = d.clone();
                disk.scanned = scanned.get(d.name.as_str()).copied();
                disk
            })
            .collect();
        let status = Status {
            version: env!("CARGO_PKG_VERSION").into(),
            updated: now(),
            doing: self.doing.clone(),
            last_round: self.round.as_ref().map(|r| r.taken),
            next_round: Some(
                now()
                    + self
                        .next_round
                        .saturating_duration_since(Instant::now())
                        .as_secs() as i64,
            ),
            skipped: self.round.as_ref().and_then(|r| r.skipped.clone()),
            errors: self
                .round
                .as_ref()
                .map(|r| r.errors.clone())
                .unwrap_or_default(),
            hub,
            report,
            quarantine: self.quarantine().list(),
            settings: serde_json::to_value(&self.config).unwrap_or_default(),
        };
        let _ = std::fs::create_dir_all(&self.dirs.data);
        let text = serde_json::to_vec(&status).unwrap_or_default();
        if let Err(err) = write_atomic(&self.dirs.data.join("status.json"), &text) {
            log(&format!("couldn't write the status: {err}"));
        }
    }
}

/// What the Hub is told about the server.
pub fn device_info(system: &System, name: &str, state: Option<&AgentState>) -> DeviceInfo {
    DeviceInfo {
        name: name.to_string(),
        platform: Platform::Linux,
        os: format!("Unraid {}", system.version()),
        hostname: disktree_agent::hostname(),
        app: format!("DiskTree for Unraid {}", env!("CARGO_PKG_VERSION")),
        actions: vec![
            ActionKind::Delete,
            ActionKind::Restore,
            ActionKind::Empty,
            ActionKind::Docker,
        ],
        attended: false,
        approver: state.and_then(|s| s.approver.as_ref().map(|a| a.key.fingerprint())),
        grants: state.map(|s| s.grants.clone()).unwrap_or_default(),
    }
}

/// Asks the Hub for news while paired; pairing later is picked up.
fn poll_hub(state_path: PathBuf, events: mpsc::Sender<Event>) {
    let transport = UreqTransport::new();
    let mut first = true;
    let mut backoff = Duration::from_secs(5);
    loop {
        let state = match AgentState::load(&state_path) {
            Ok(Some(state)) => state,
            _ => {
                first = true;
                std::thread::sleep(Duration::from_secs(20));
                continue;
            }
        };
        let client = Client::new(&state, &transport);
        let result = if first {
            client.poll_all(25)
        } else {
            client.poll(25)
        };
        let failed = result.is_err();
        let removed = matches!(&result, Err(e) if e.removed);
        if failed {
            first = true;
        } else {
            first = false;
            backoff = Duration::from_secs(5);
        }
        if events.send(Event::Polled(result)).is_err() {
            return;
        }
        if removed {
            std::thread::sleep(Duration::from_secs(300));
        } else if failed {
            std::thread::sleep(backoff);
            backoff = (backoff * 2).min(Duration::from_secs(300));
        }
    }
}

fn daemon(dirs: &Dirs) -> Result<(), String> {
    let system = &dirs.system;
    if !system.is_unraid() {
        return Err("this isn't an Unraid server".into());
    }
    std::fs::create_dir_all(dirs.requests()).map_err(|e| e.to_string())?;
    let tops: BTreeMap<String, DiskTops> = std::fs::read_to_string(dirs.config.join("disks.json"))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let (events_tx, events) = mpsc::channel();
    let mut daemon = Daemon {
        dirs: Dirs {
            config: dirs.config.clone(),
            data: dirs.data.clone(),
            system: system.clone(),
        },
        config: read_config(dirs),
        tops,
        round: None,
        round_running: false,
        next_round: Instant::now() + Duration::from_secs(30),
        monitor: Monitor::default(),
        open: BTreeMap::new(),
        // Moves running when the daemon stopped were cut short.
        moves: std::fs::read_to_string(dirs.data.join("moves.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<MoveJob>>(&t).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|mut job| {
                if job.state == "running" || job.state == "planned" {
                    job.state = "failed".into();
                    job.detail = "stopped when DiskTree restarted; the copies made so far are on \
                                  the target disk, the originals untouched"
                        .into();
                } else if job.state == "queued" {
                    job.state = "cancelled".into();
                    job.detail = "not started: DiskTree restarted".into();
                }
                job
            })
            .collect(),
        cancels: HashMap::new(),
        doing: "starting".into(),
        connection: String::new(),
        pending: HashMap::new(),
        events: events_tx.clone(),
    };
    log(&format!(
        "DiskTree {} for Unraid {} started",
        env!("CARGO_PKG_VERSION"),
        system.version()
    ));
    {
        let state_path = dirs.state();
        let events = events_tx.clone();
        std::thread::Builder::new()
            .name("disktree-hub".into())
            .spawn(move || poll_hub(state_path, events))
            .map_err(|e| e.to_string())?;
    }
    let mut next_sample = Instant::now();
    let mut next_hub_refresh = Instant::now() + Duration::from_secs(3600);
    daemon.sample();
    daemon.doing = "idle".into();
    daemon.write_status();
    loop {
        if Instant::now() >= daemon.next_round {
            daemon.config = read_config(dirs);
            daemon.next_round =
                Instant::now() + Duration::from_secs(daemon.config.scan_hours * 3600);
            daemon.start_round();
        }
        if Instant::now() >= next_sample {
            // The array was stopped at the last round and has started
            // since: look now rather than at the next scheduled round.
            if !daemon.round_running
                && daemon.round.as_ref().is_some_and(|r| r.skipped.is_some())
                && daemon.array().started
            {
                daemon.start_round();
            }
            daemon.sample();
            next_sample = Instant::now() + Duration::from_secs(60);
            daemon.write_status();
        }
        if Instant::now() >= next_hub_refresh {
            next_hub_refresh = Instant::now() + Duration::from_secs(3600);
            let array = daemon.array();
            let unraid = daemon.unraid_report();
            daemon.report(Report {
                disks: Some(round::hub_disks(&array)),
                unraid: Some(unraid),
                ..Report::default()
            });
        }
        daemon.take_requests();
        match events.recv_timeout(Duration::from_secs(2)) {
            Ok(Event::Progress(what)) => {
                daemon.doing = what;
                daemon.write_status();
            }
            Ok(Event::Round(found)) => {
                daemon.round_running = false;
                daemon.tops = found.tops.clone();
                let _ = std::fs::create_dir_all(&dirs.config);
                let _ = write_atomic(
                    &daemon.tops_path(),
                    &serde_json::to_vec(&daemon.tops).unwrap_or_default(),
                );
                for err in &found.errors {
                    log(err);
                }
                log(&match &found.skipped {
                    Some(why) => format!("not scanning: {why}"),
                    None => format!("scanned {} disks", found.scans.len()),
                });
                daemon.round = Some(found);
                daemon.doing = "idle".into();
                daemon.report_round();
                daemon.write_status();
            }
            Ok(Event::Polled(Ok(response))) => {
                daemon.connection = "connected".into();
                daemon.handle_poll(response);
            }
            Ok(Event::Polled(Err(err))) => {
                daemon.connection = if err.removed {
                    "removed from the Hub: pair again".into()
                } else {
                    format!("can't reach the Hub: {err}")
                };
            }
            Ok(Event::Move(job, finished)) => {
                if finished {
                    daemon.cancels.remove(&job.id);
                    log(&format!("move {}: {}", job.id, job.detail));
                }
                match daemon.moves.iter_mut().find(|m| m.id == job.id) {
                    Some(existing) => *existing = job,
                    None => daemon.moves.push(job),
                }
                if finished {
                    daemon.next_move();
                }
                daemon.save_moves();
                daemon.write_status();
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}
