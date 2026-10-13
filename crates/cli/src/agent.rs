//! `freesize agent` and `freesize approver`: this machine as a device of a
//! FreeSize Hub, for servers and other machines nobody sits at. Requests
//! from the Hub run only after the approver set here allows them, from
//! their own device.

use std::collections::HashMap;
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use clap::Subcommand;
use freesize_agent::commands::{self, Executor, Guards, Policy, Request, Verdict};
use freesize_agent::guard::OpenFiles;
use freesize_agent::quarantine::{MountPlaces, Quarantine, QuarantineExecutor};
use freesize_agent::{AgentState, Approver, Client, Transport, UreqTransport, now};
use freesize_core::{ScanOptions, ScanProgress, format_size, scan};
use freesize_proto::pairing::{format_code, normalize_hub};
use freesize_proto::{
    ActionKind, ApprovedBy, CommandState, CommandUpdate, DeviceInfo, PairingLink, Platform, Report,
};

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Connect this machine to a FreeSize Hub, report to it, and carry out
    /// requests its approver allows.
    Agent {
        #[command(subcommand)]
        action: AgentAction,
        /// File keeping the pairing (default: in FreeSize's data folder).
        #[arg(long, global = true, env = "FREESIZE_AGENT_STATE", value_name = "FILE")]
        state: Option<PathBuf>,
    },
    /// Choose the device that approves the Hub's requests for this machine.
    /// It can only be set here, never from the Hub.
    Approver {
        #[command(subcommand)]
        action: ApproverAction,
        #[arg(long, global = true, env = "FREESIZE_AGENT_STATE", value_name = "FILE")]
        state: Option<PathBuf>,
    },
    /// The release key: make it, sign a release's sums, check a download.
    Release {
        #[command(subcommand)]
        action: crate::release::ReleaseAction,
    },
    /// Check for a newer release, and install it over this program.
    Update(crate::update::UpdateArgs),
    /// The Unraid plugin's background work, and the commands its page uses.
    #[cfg(unix)]
    Unraid {
        #[command(subcommand)]
        action: crate::unraid::UnraidAction,
        #[command(flatten)]
        dirs: crate::unraid::DirArgs,
    },
}

#[derive(Subcommand, Debug)]
pub enum AgentAction {
    /// Pair with a Hub, using the link from its dashboard (Pair a device), or
    /// its address and the code.
    Pair {
        /// The pairing link, `freesize://pair?...`.
        link: Option<String>,
        /// The Hub's address, when typing the code instead of the link.
        #[arg(long)]
        hub: Option<String>,
        #[arg(long)]
        code: Option<String>,
        /// The Hub's key fingerprint, to check it without being asked.
        #[arg(long)]
        fingerprint: Option<String>,
        /// The name the Hub shows (default: the host name).
        #[arg(long)]
        name: Option<String>,
    },
    /// Report to the Hub and carry out approved requests, until stopped.
    Run {
        /// Folders to scan and report; requests to delete stay inside them.
        #[arg(long = "scan", value_name = "FOLDER")]
        scans: Vec<PathBuf>,
        /// Hours between scans.
        #[arg(long, default_value_t = 24)]
        every: u64,
        /// Report once and stop, without waiting for requests.
        #[arg(long)]
        once: bool,
    },
    /// Show the Hub, this machine's key, the approver and grants.
    Status,
    /// End all grants now.
    Revoke,
    /// Forget the Hub. Remove the device in the dashboard as well.
    Unpair,
}

#[derive(Subcommand, Debug)]
pub enum ApproverAction {
    /// Set the approver: the key fingerprint shown in the FreeSize app on
    /// that device (Hub → This device's key).
    Set {
        fingerprint: String,
        /// Don't ask to confirm.
        #[arg(long)]
        yes: bool,
        /// Only show which device that is; change nothing.
        #[arg(long, conflicts_with = "yes")]
        check: bool,
    },
    /// Show the approver.
    Show,
    /// Remove the approver; requests are then refused.
    Clear,
}

fn default_state_path() -> PathBuf {
    // On Unraid, the plugin's pairing on the flash drive: root's home is in
    // memory, and the plugin only reads its own file.
    #[cfg(unix)]
    if freesize_unraid::System::real().is_unraid() {
        return crate::unraid::plugin_state();
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|p| PathBuf::from(p).join("FreeSize"))
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME")
            .map(|h| PathBuf::from(h).join("Library/Application Support/FreeSize"))
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        Some(PathBuf::from(xdg).join("freesize"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share/freesize"))
    };
    base.map_or_else(|| PathBuf::from("."), freesize_core::renamed::adopt_dir)
        .join("agent.json")
}

pub fn main(command: Command) -> ExitCode {
    let result = match command {
        Command::Agent { action, state } => {
            let path = state.unwrap_or_else(default_state_path);
            match action {
                AgentAction::Pair {
                    link,
                    hub,
                    code,
                    fingerprint,
                    name,
                } => pair(&path, link, hub, code, fingerprint, name),
                AgentAction::Run { scans, every, once } => run(&path, &scans, every, once),
                AgentAction::Status => status(&path),
                AgentAction::Revoke => revoke(&path),
                AgentAction::Unpair => unpair(&path),
            }
        }
        Command::Approver { action, state } => {
            let path = state.unwrap_or_else(default_state_path);
            match action {
                ApproverAction::Set {
                    fingerprint,
                    yes,
                    check,
                } => set_approver(&path, &fingerprint, yes, check),
                ApproverAction::Show => status(&path),
                ApproverAction::Clear => clear_approver(&path),
            }
        }
        Command::Release { action } => crate::release::main(action),
        Command::Update(args) => crate::update::main(args),
        #[cfg(unix)]
        Command::Unraid { action, dirs } => crate::unraid::main(action, dirs),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("freesize: {err}");
            ExitCode::FAILURE
        }
    }
}

fn load(path: &Path) -> Result<AgentState, String> {
    AgentState::load(path)?.ok_or_else(|| {
        format!(
            "not paired with a Hub yet ({} doesn't exist): run `freesize agent pair` first",
            path.display()
        )
    })
}

fn ask(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().lock().read_line(&mut answer);
    matches!(answer.trim(), "y" | "Y" | "yes" | "Yes")
}

fn os_name() -> String {
    if cfg!(windows) {
        return "Windows".into();
    }
    if cfg!(target_os = "macos") {
        return "macOS".into();
    }
    if let Ok(text) = std::fs::read_to_string("/etc/unraid-version")
        && let Some(version) = text.split('"').nth(1)
    {
        return format!("Unraid {version}");
    }
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix("PRETTY_NAME="))
                .map(|v| v.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into())
}

fn device_info(name: &str, state: Option<&AgentState>) -> DeviceInfo {
    // On Unraid the plugin's daemon reports; say the same as it does, so
    // pairing or setting the approver here doesn't change what the Hub shows.
    #[cfg(unix)]
    {
        let system = freesize_unraid::System::real();
        if system.is_unraid() {
            return crate::unraid::device_info(&system, name, state);
        }
    }
    DeviceInfo {
        name: name.to_string(),
        platform: Platform::current(),
        os: os_name(),
        hostname: freesize_agent::hostname(),
        app: format!("FreeSize agent {}", env!("CARGO_PKG_VERSION")),
        actions: vec![ActionKind::Delete, ActionKind::Restore, ActionKind::Empty],
        attended: false,
        approver: state.and_then(|s| s.approver.as_ref().map(|a| a.key.fingerprint())),
        grants: state.map(|s| s.grants.clone()).unwrap_or_default(),
        rights: freesize_core::access::rights().label().into(),
    }
}

fn pair(
    path: &Path,
    link: Option<String>,
    hub: Option<String>,
    code: Option<String>,
    fingerprint: Option<String>,
    name: Option<String>,
) -> Result<(), String> {
    if AgentState::load(path)?.is_some() {
        return Err(format!(
            "already paired ({}): run `freesize agent unpair` first",
            path.display()
        ));
    }
    let mut link = match (link, hub, code) {
        (Some(link), _, _) => {
            PairingLink::parse(&link).ok_or("that isn't a FreeSize pairing link")?
        }
        (None, Some(hub), Some(code)) => PairingLink {
            hub: normalize_hub(&hub).ok_or("that isn't a Hub address")?,
            code: format_code(&code).ok_or("a pairing code has 12 letters and digits")?,
            key: None,
        },
        _ => return Err("give the pairing link, or --hub and --code".into()),
    };
    if link.key.is_none() {
        link.key = fingerprint.map(|f| f.trim().to_uppercase());
    }
    let name = name.unwrap_or_else(freesize_agent::hostname);
    let transport = UreqTransport::new();
    let mut confirm = |hello: &freesize_proto::Hello| {
        println!("The Hub \"{}\" at {} has the key", hello.name, link.hub);
        println!("    {}", hello.key.fingerprint());
        println!("Check it matches the dashboard (Settings → This Hub).");
        ask("Is it the same?")
    };
    let state = freesize_agent::pair(&transport, &link, device_info(&name, None), &mut confirm)
        .map_err(|e| e.message)?;
    state.save(path)?;
    println!("Paired with {} as \"{name}\".", state.hub_name);
    println!("This machine's key: {}", state.fingerprint());
    if freesize_proto::pairing::plain_http_over_network(&state.hub) {
        eprintln!(
            "warning: {} is plain http, so what this machine reports (folder and file names) \
             can be read on the network. Update the Hub to 0.8.7 or newer, or put HTTPS in \
             front of it.",
            state.hub
        );
    } else if state.ca.is_some() {
        println!(
            "Over HTTPS with the Hub's own certificate, trusted for {} only.",
            state.hub
        );
    }
    println!();
    #[cfg(unix)]
    if freesize_unraid::System::real().is_unraid() {
        unraid_next(path);
        return Ok(());
    }
    println!("Next:");
    println!("  1. Choose who approves requests for it, with the key shown in the FreeSize");
    println!("     app on their phone:   freesize approver set XXXX-XXXX-XXXX-XXXX");
    println!("  2. Start reporting:      freesize agent run --scan /path/to/folder");
    Ok(())
}

/// What to do after pairing on Unraid, where the plugin reports on its own.
#[cfg(unix)]
fn unraid_next(path: &Path) {
    let plugin = crate::unraid::plugin_state();
    if path != plugin {
        println!(
            "The FreeSize plugin only uses {}, so it won't report with this pairing.",
            plugin.display()
        );
        println!(
            "Run `freesize agent unpair --state {}` and pair again without --state.",
            path.display()
        );
        return;
    }
    if crate::unraid::tell_daemon_paired() {
        println!("The plugin now reports this server to the Hub on its own.");
    }
    println!("Next, choose who approves the Hub's requests, by the key the FreeSize app");
    println!("on their phone shows: on the plugin's Settings and Hub tab, or with");
    println!("  freesize approver set XXXX-XXXX-XXXX-XXXX");
}

fn status(path: &Path) -> Result<(), String> {
    let mut state = load(path)?;
    println!("Hub:          {} ({})", state.hub_name, state.hub);
    println!("Hub key:      {}", state.hub_key.fingerprint());
    println!(
        "This device:  {} (key {})",
        state.device,
        state.fingerprint()
    );
    match &state.approver {
        Some(a) => println!("Approver:     {} (key {})", a.name, a.key.fingerprint()),
        None => println!("Approver:     none: every request from the Hub is refused"),
    }
    let grants = state.active_grants(now()).to_vec();
    if grants.is_empty() {
        println!("Grants:       none");
    }
    for g in grants {
        println!(
            "Grant:        {} may {} for {} more minutes",
            g.admin_name,
            g.kind.label(),
            (g.expires - now()).max(0) / 60
        );
    }
    Ok(())
}

/// Tells the Hub what changed about this device.
fn report_info(state: &AgentState, transport: &dyn Transport) {
    let report = Report {
        info: Some(device_info(&state.name, Some(state))),
        ..Report::default()
    };
    if let Err(err) = Client::new(state, transport).report(&report) {
        eprintln!("freesize: couldn't tell the Hub ({err}); it learns on the next report");
    }
}

fn revoke(path: &Path) -> Result<(), String> {
    let mut state = load(path)?;
    let ended = state.revoke(None, None);
    state.save(path)?;
    report_info(&state, &UreqTransport::new());
    println!("Ended {ended} grant(s).");
    Ok(())
}

fn unpair(path: &Path) -> Result<(), String> {
    let state = load(path)?;
    std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    println!(
        "Forgot {}. Remove \"{}\" from the dashboard too, so its reports go away.",
        state.hub_name, state.device
    );
    Ok(())
}

fn set_approver(path: &Path, fingerprint: &str, yes: bool, check: bool) -> Result<(), String> {
    let mut state = load(path)?;
    let transport = UreqTransport::new();
    let found = Client::new(&state, &transport)
        .device_key(fingerprint)
        .map_err(|e| e.message)?;
    println!(
        "Requests from the Hub for this machine will go to \"{}\" ({}), key {}.",
        found.name,
        found.platform.label(),
        found.key.fingerprint()
    );
    if check {
        return Ok(());
    }
    if !yes && !ask("Set it as approver?") {
        return Err("not changed".into());
    }
    state.approver = Some(Approver {
        device: found.id,
        name: found.name.clone(),
        key: found.key,
    });
    // A new approver starts without grants from the old one.
    state.revoke(None, None);
    state.save(path)?;
    report_info(&state, &transport);
    println!("\"{}\" approves requests for this machine now.", found.name);
    Ok(())
}

fn clear_approver(path: &Path) -> Result<(), String> {
    let mut state = load(path)?;
    state.approver = None;
    state.revoke(None, None);
    state.save(path)?;
    report_info(&state, &UreqTransport::new());
    println!("No approver: the Hub's requests are refused until one is set.");
    Ok(())
}

pub(crate) fn log(message: &str) {
    println!("{message}");
}

enum ScanEvent {
    Done(Box<freesize_proto::ScanReport>),
    Failed(String),
    Finished,
}

/// Scans the folders one after another on a thread of its own.
fn start_scans(roots: Vec<PathBuf>, events: mpsc::Sender<ScanEvent>) {
    std::thread::spawn(move || {
        for root in roots {
            log(&format!("scanning {}", root.display()));
            match scan(&root, &ScanOptions::default(), &ScanProgress::new()) {
                Ok(result) => {
                    let label = root.file_name().map_or_else(
                        || root.display().to_string(),
                        |n| n.to_string_lossy().into_owned(),
                    );
                    let report = freesize_agent::report::scan_report(
                        &result.tree,
                        &label,
                        now(),
                        result.errors,
                        !result.cancelled,
                    );
                    let _ = events.send(ScanEvent::Done(Box::new(report)));
                }
                Err(err) => {
                    let _ = events.send(ScanEvent::Failed(format!("{}: {err}", root.display())));
                }
            }
        }
        let _ = events.send(ScanEvent::Finished);
    });
}

fn run(path: &Path, scans: &[PathBuf], every: u64, once: bool) -> Result<(), String> {
    let mut state = load(path)?;
    let transport = UreqTransport::new();
    let mut roots = Vec::new();
    for folder in scans {
        roots.push(
            folder
                .canonicalize()
                .map_err(|e| format!("{}: {e}", folder.display()))?,
        );
    }
    let keep = vec![path.canonicalize().unwrap_or_else(|_| path.to_path_buf())];
    let policy = Policy {
        attended: false,
        actions: vec![ActionKind::Delete, ActionKind::Restore, ActionKind::Empty],
        roots: roots.clone(),
        keep,
        guards: Guards(vec![Arc::new(OpenFiles::default())]),
    };
    // Deletes go to a quarantine on the same filesystem, to put back or
    // empty by later requests.
    let executor = QuarantineExecutor {
        quarantine: Quarantine::new(Box::new(MountPlaces::new(&roots))),
        device: state.name.clone(),
    };
    log(&format!(
        "FreeSize agent {} reporting to {} ({})",
        env!("CARGO_PKG_VERSION"),
        state.hub_name,
        state.hub
    ));
    match &state.approver {
        Some(a) => log(&format!("requests are approved by {}", a.name)),
        None => log("no approver set: requests from the Hub are refused"),
    }

    let places = freesize_agent::report::system_places();
    let (events_tx, events) = mpsc::channel();
    let mut next_scan = Instant::now();
    let mut next_disks = Instant::now();
    let mut scanning = false;
    let mut scanned_once = roots.is_empty();
    let mut pending: HashMap<String, Request> = HashMap::new();
    let mut backoff = Duration::from_secs(5);
    // Until the first poll goes through, ask for everything still open, in
    // case requests were waiting on this agent before it restarted.
    let mut first_poll = true;

    loop {
        let client = Client::new(&state, &transport);
        let mut sent = Ok(());
        if Instant::now() >= next_disks {
            sent = client.report(&Report {
                info: Some(device_info(&state.name, Some(&state))),
                disks: Some(freesize_agent::report::disks(&places)),
                quarantine: Some(executor.quarantine.list()),
                ..Report::default()
            });
            if sent.is_ok() {
                next_disks = Instant::now() + Duration::from_secs(3600);
            }
        }
        if !scanning && !roots.is_empty() && Instant::now() >= next_scan {
            scanning = true;
            start_scans(roots.clone(), events_tx.clone());
        }
        while let Ok(event) = events.try_recv() {
            match event {
                ScanEvent::Done(report) => {
                    let size = format_size(report.allocated);
                    match client.report(&Report {
                        scan: Some(*report),
                        ..Report::default()
                    }) {
                        Ok(()) => log(&format!("reported a scan ({size})")),
                        Err(err) => log(&format!("couldn't report a scan: {err}")),
                    }
                }
                ScanEvent::Failed(err) => log(&format!("scan failed: {err}")),
                ScanEvent::Finished => {
                    scanning = false;
                    scanned_once = true;
                    next_scan = Instant::now() + Duration::from_secs(every.max(1) * 3600);
                }
            }
        }
        if once {
            if let Err(err) = sent {
                return Err(err.message);
            }
            if scanned_once && !scanning {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }

        let polled = sent.and_then(|()| {
            if first_poll {
                client.poll_all(25)
            } else {
                client.poll(25)
            }
        });
        first_poll &= polled.is_err();
        let response = match polled {
            Ok(response) => {
                backoff = Duration::from_secs(5);
                response
            }
            Err(err) => {
                if err.removed {
                    return Err(format!(
                        "{err}. Run `freesize agent unpair` and pair again."
                    ));
                }
                let wait = if err.temporary() {
                    backoff
                } else {
                    Duration::from_secs(60)
                };
                log(&format!("{err}; trying again in {}s", wait.as_secs()));
                std::thread::sleep(wait);
                backoff = (backoff * 2).min(Duration::from_secs(300));
                continue;
            }
        };
        let outcome = handle(
            &mut state,
            &transport,
            &policy,
            &executor,
            &mut pending,
            response,
        );
        if outcome.changed {
            state.save(path)?;
        }
        if outcome.ran {
            // The quarantine changed: say so, for the Hub's buttons.
            let report = Report {
                quarantine: Some(executor.quarantine.list()),
                ..Report::default()
            };
            if let Err(err) = Client::new(&state, &transport).report(&report) {
                log(&format!("couldn't report the quarantine: {err}"));
            }
        }
    }
}

pub(crate) fn send(
    state: &AgentState,
    transport: &dyn Transport,
    id: &str,
    update: &CommandUpdate,
) {
    if let Err(err) = Client::new(state, transport).update(id, update) {
        log(&format!("couldn't tell the Hub about request {id}: {err}"));
    }
}

fn refusal(state: CommandState, reason: String) -> CommandUpdate {
    CommandUpdate {
        state,
        detail: reason,
        approval: None,
        freed: 0,
        failed: Vec::new(),
    }
}

/// What handling a poll did.
#[derive(Default)]
pub struct Outcome {
    /// The saved state changed (grants, handled requests).
    pub changed: bool,
    /// A request was carried out.
    pub ran: bool,
}

/// Acts on what a poll brought.
pub fn handle(
    state: &mut AgentState,
    transport: &dyn Transport,
    policy: &Policy,
    executor: &dyn Executor,
    pending: &mut HashMap<String, Request>,
    response: freesize_proto::PollResponse,
) -> Outcome {
    let mut changed = false;
    let mut ran = false;
    // The Hub serves HTTPS now: follow it there, once it answers there with
    // the key this machine paired with.
    if let Some(signed) = &response.move_to {
        match freesize_agent::move_to(transport, state, signed) {
            Ok(true) => {
                log(&format!("the Hub is at {} now; reporting there", state.hub));
                changed = true;
            }
            Ok(false) => {}
            Err(err) => log(&format!("the Hub asked to move: {err}")),
        }
    }
    for id in &response.cancelled {
        if pending.remove(id).is_some() {
            log(&format!("request {id} was withdrawn"));
        }
    }
    pending.retain(|id, request| {
        let alive = request.command.expires > now();
        if !alive {
            send(
                state,
                transport,
                id,
                &refusal(
                    CommandState::Expired,
                    "the approver didn't answer in time".into(),
                ),
            );
        }
        alive
    });
    for signed in &response.commands {
        match commands::consider(state, signed, policy, now()) {
            Verdict::Ignore => {}
            Verdict::Refuse {
                id,
                state: outcome,
                reason,
            } => {
                log(&format!("refused request {id}: {reason}"));
                send(state, transport, &id, &refusal(outcome, reason));
            }
            Verdict::Run(request, approval) => {
                log(&format!(
                    "running request {} under a grant: {}",
                    request.command.id,
                    request.command.action.summary()
                ));
                let update = commands::run(&request, executor, approval);
                log(&format!(
                    "request {}: {}",
                    request.command.id, update.detail
                ));
                state.mark_handled(&request.command.id);
                changed = true;
                ran = true;
                send(state, transport, &request.command.id, &update);
            }
            Verdict::AskApprover(request) | Verdict::Ask(request) => {
                if pending.contains_key(&request.command.id) {
                    continue;
                }
                log(&format!(
                    "asking the approver about request {}: {}",
                    request.command.id,
                    request.command.action.summary()
                ));
                // How this device carries it out, for the approver.
                send(
                    state,
                    transport,
                    &request.command.id,
                    &refusal(CommandState::NeedsApprover, executor.note(&request)),
                );
                pending.insert(request.command.id.clone(), request);
            }
        }
    }
    for signed in &response.decisions {
        let Ok(decision) = signed.peek() else {
            continue;
        };
        let Some(request) = pending.get(&decision.command).cloned() else {
            continue;
        };
        match commands::check_decision(state, &request, signed) {
            Ok(choice) => {
                pending.remove(&decision.command);
                let fingerprint = state
                    .approver
                    .as_ref()
                    .map(|a| a.key.fingerprint())
                    .unwrap_or_default();
                let by = ApprovedBy::Approver { fingerprint };
                changed = true;
                match commands::decide(state, &request, choice, by, now()) {
                    Some(approval) => {
                        log(&format!(
                            "the approver allowed request {}",
                            request.command.id
                        ));
                        let update = commands::run(&request, executor, approval);
                        log(&format!(
                            "request {}: {}",
                            request.command.id, update.detail
                        ));
                        state.mark_handled(&request.command.id);
                        ran = true;
                        send(state, transport, &request.command.id, &update);
                    }
                    None => {
                        log(&format!(
                            "the approver denied request {}",
                            request.command.id
                        ));
                        send(
                            state,
                            transport,
                            &request.command.id,
                            &refusal(CommandState::Denied, "denied by the approver".into()),
                        );
                    }
                }
            }
            Err(err) => log(&format!(
                "ignored a decision about request {}: {err}",
                decision.command
            )),
        }
    }
    Outcome { changed, ran }
}
