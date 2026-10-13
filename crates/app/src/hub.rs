//! The app as a device of a DiskTree Hub: connecting with a pairing link,
//! reporting after scans, and asking here before doing anything the Hub
//! requests. The app can also be the designated approver of servers: their
//! requests come here too, and the answer is signed with this device's key.
//!
//! A worker thread owns the connection; the controller talks to it through
//! channels and picks up its events on the UI timer.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use disktree_agent::commands::{self, Choice, Executor, Policy, Request, Verdict};
use disktree_agent::{AgentError, AgentState, Client, Transport, now};
use disktree_proto::{
    ActionKind, ApprovalRequest, ApprovedBy, Command, CommandState, CommandUpdate, Decision,
    DeviceInfo, Hello, PairingLink, Platform, PollResponse, Purpose, Report,
};

use crate::cleanup::Junk;

/// What the controller is told.
pub enum Event {
    /// How the connection is going, for the Hub card.
    Status(String),
    /// Something to approve or deny here.
    Ask(Prompt),
    /// A request the Hub withdrew.
    Withdrawn(String),
    /// A request was carried out (or failed): what happened, and the paths
    /// it deleted, to take out of an open scan.
    Done { text: String, deleted: Vec<PathBuf> },
    /// The Hub removed this device.
    Removed,
    /// Pairing: the Hub to compare by fingerprint before going on.
    Confirm(Hello),
    /// Pairing finished: the Hub's name, or why not.
    Paired(Result<String, String>),
}

/// A request shown for approval.
#[derive(Clone, Debug)]
pub struct Prompt {
    pub id: String,
    pub title: String,
    pub lines: Vec<String>,
    /// Grant lengths to offer, in minutes.
    pub minutes: Vec<u32>,
    kind: PromptKind,
}

#[derive(Clone, Debug)]
enum PromptKind {
    /// For this device.
    Own(Box<Request>),
    /// For a device this one approves for.
    ForOther {
        request: Box<ApprovalRequest>,
        command: Box<Command>,
    },
}

enum Job {
    Report(Box<Report>),
    Answer(Box<Prompt>, Choice),
    Revoke,
    /// What a poll brought, from the polling thread.
    Polled(Result<PollResponse, AgentError>),
    Stop,
}

/// Sends reports from any thread (the scan worker uses it).
#[derive(Clone)]
pub struct Reporter(Option<Sender<Job>>);

impl Reporter {
    pub fn send(&self, report: Report) {
        if let Some(jobs) = &self.0 {
            let _ = jobs.send(Job::Report(Box::new(report)));
        }
    }

    pub fn connected(&self) -> bool {
        self.0.is_some()
    }
}

type SharedTransport = Arc<dyn Transport + Send + Sync>;
type SharedExecutor = Arc<dyn Executor + Send + Sync>;

pub struct Hub {
    path: PathBuf,
    state: Arc<Mutex<Option<AgentState>>>,
    policy: Arc<Mutex<Policy>>,
    jobs: Option<Sender<Job>>,
    events_tx: Sender<Event>,
    events: Receiver<Event>,
    transport: SharedTransport,
    executor: SharedExecutor,
}

impl Hub {
    /// Loads the pairing from `path` and starts reporting if there is one.
    pub fn new(
        path: PathBuf,
        transport: SharedTransport,
        executor: SharedExecutor,
        policy: Policy,
    ) -> Hub {
        let (events_tx, events) = mpsc::channel();
        let state = match AgentState::load(&path) {
            Ok(state) => state,
            Err(err) => {
                let _ = events_tx.send(Event::Status(format!(
                    "Couldn't read the Hub pairing: {err}"
                )));
                None
            }
        };
        let mut hub = Hub {
            path,
            state: Arc::new(Mutex::new(state)),
            policy: Arc::new(Mutex::new(policy)),
            jobs: None,
            events_tx,
            events,
            transport,
            executor,
        };
        hub.start();
        hub
    }

    fn start(&mut self) {
        if self.state().is_none() {
            return;
        }
        let (jobs, inbox) = mpsc::channel();
        self.jobs = Some(jobs.clone());
        let worker = Worker {
            path: self.path.clone(),
            state: self.state.clone(),
            policy: self.policy.clone(),
            transport: self.transport.clone(),
            executor: self.executor.clone(),
            events: self.events_tx.clone(),
            asked: HashSet::new(),
        };
        let spawned = std::thread::Builder::new()
            .name("disktree-hub".into())
            .spawn(move || worker.run(jobs, inbox));
        if spawned.is_err() {
            self.jobs = None;
        }
    }

    /// A copy of the pairing, if there is one.
    pub fn state(&self) -> Option<AgentState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn reporter(&self) -> Reporter {
        Reporter(self.jobs.clone())
    }

    pub fn report(&self, report: Report) {
        self.reporter().send(report);
    }

    /// Folders scanned on this device; deletes the Hub asks for stay inside
    /// them (and the drives the app lists).
    pub fn add_root(&self, root: &Path) {
        let mut policy = self.policy.lock().unwrap_or_else(|e| e.into_inner());
        if !policy.roots.iter().any(|r| root.starts_with(r)) {
            policy.roots.push(root.to_path_buf());
        }
    }

    /// Pairs with the Hub in `link`, on a thread. Without the Hub's key in
    /// the link, the controller gets `Event::Confirm` first and calls this
    /// again with `confirmed`.
    pub fn connect(&self, link: PairingLink, info: DeviceInfo, confirmed: Option<Hello>) {
        let events = self.events_tx.clone();
        let transport = self.transport.clone();
        let path = self.path.clone();
        let state = self.state.clone();
        if disktree_proto::pairing::plain_http_over_network(&link.hub) {
            let _ = events.send(Event::Status(format!(
                "{} is plain http: what this device reports (folder and file names) can be read \
                 on the network. Put HTTPS in front of the Hub.",
                link.hub
            )));
        }
        std::thread::spawn(move || {
            let mut asked = false;
            let mut confirm = |hello: &Hello| match &confirmed {
                Some(expected) => expected.key == hello.key,
                None => {
                    asked = true;
                    let _ = events.send(Event::Confirm(hello.clone()));
                    false
                }
            };
            match disktree_agent::pair(&*transport, &link, info, &mut confirm) {
                Ok(paired) => {
                    let name = paired.hub_name.clone();
                    let saved = paired.save(&path);
                    *state.lock().unwrap_or_else(|e| e.into_inner()) = Some(paired);
                    let _ = events.send(Event::Paired(saved.map(|()| name)));
                }
                // Waiting for the person to compare fingerprints.
                Err(_) if asked => {}
                Err(err) => {
                    let _ = events.send(Event::Paired(Err(err.message)));
                }
            }
        });
    }

    /// Starts reporting after pairing.
    pub fn connected(&mut self) {
        if self.jobs.is_none() {
            self.start();
        }
    }

    pub fn answer(&self, prompt: Prompt, choice: Choice) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Answer(Box::new(prompt), choice));
        }
    }

    pub fn revoke_grants(&self) {
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job::Revoke);
        }
    }

    /// Forgets the Hub. The device stays in the dashboard until an admin
    /// removes it.
    pub fn disconnect(&mut self) {
        if let Some(jobs) = self.jobs.take() {
            let _ = jobs.send(Job::Stop);
        }
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let _ = std::fs::remove_file(&self.path);
    }

    /// Events since the last call.
    pub fn events(&self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            out.push(event);
        }
        out
    }
}

/// What this device says about itself.
pub fn device_info(name: &str, os: &str, state: Option<&AgentState>) -> DeviceInfo {
    let platform = Platform::current();
    DeviceInfo {
        name: name.to_string(),
        platform,
        os: os.to_string(),
        hostname: disktree_agent::hostname(),
        app: format!("DiskTree app {}", env!("CARGO_PKG_VERSION")),
        actions: if platform == Platform::Android {
            vec![ActionKind::Delete, ActionKind::Uninstall, ActionKind::Clean]
        } else {
            vec![ActionKind::Delete]
        },
        attended: true,
        approver: None,
        grants: state.map(|s| s.grants.clone()).unwrap_or_default(),
    }
}

struct Worker {
    path: PathBuf,
    state: Arc<Mutex<Option<AgentState>>>,
    policy: Arc<Mutex<Policy>>,
    transport: SharedTransport,
    executor: SharedExecutor,
    events: Sender<Event>,
    /// Prompts already shown, so a redelivered request isn't shown twice.
    asked: HashSet<String>,
}

/// Asks the Hub for news, over and over, and hands it to the worker.
struct Poller {
    state: Arc<Mutex<Option<AgentState>>>,
    transport: SharedTransport,
    jobs: Sender<Job>,
}

impl Poller {
    fn run(self) {
        let mut backoff = Duration::from_secs(5);
        // Until the first poll goes through, ask for everything still open:
        // what was shown before the app closed comes back.
        let mut first = true;
        loop {
            // Gone after disconnecting.
            let Some(state) = self.state.lock().unwrap_or_else(|e| e.into_inner()).clone() else {
                return;
            };
            let client = Client::new(&state, &*self.transport);
            let result = if first {
                client.poll_all(10)
            } else {
                client.poll(10)
            };
            first &= result.is_err();
            let failed = result.is_err();
            let removed = matches!(&result, Err(err) if err.removed);
            // The worker stopped.
            if self.jobs.send(Job::Polled(result)).is_err() || removed {
                return;
            }
            if failed {
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(Duration::from_secs(120));
            } else {
                backoff = Duration::from_secs(5);
            }
        }
    }
}

impl Worker {
    fn snapshot(&self) -> Option<AgentState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn save(&self) {
        if let Some(state) = self.snapshot() {
            let _ = state.save(&self.path);
        }
    }

    fn tell(&self, event: Event) {
        let _ = self.events.send(event);
    }

    fn run(mut self, jobs: Sender<Job>, inbox: Receiver<Job>) {
        // Polling waits on the Hub, so it gets its own thread; reports and
        // answers go out as soon as they're made.
        let poller = Poller {
            state: self.state.clone(),
            transport: self.transport.clone(),
            jobs,
        };
        if std::thread::Builder::new()
            .name("disktree-hub-poll".into())
            .spawn(move || poller.run())
            .is_err()
        {
            self.tell(Event::Status("Couldn't start talking to the Hub".into()));
            return;
        }
        let mut connected = false;
        while let Ok(job) = inbox.recv() {
            match job {
                Job::Report(report) => self.send_report(&report),
                Job::Answer(prompt, choice) => self.answer(*prompt, choice),
                Job::Revoke => {
                    if let Some(state) = self
                        .state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .as_mut()
                    {
                        state.revoke(None, None);
                    }
                    self.save();
                }
                Job::Polled(Ok(response)) => {
                    if !connected {
                        connected = true;
                        if let Some(state) = self.snapshot() {
                            self.tell(Event::Status(format!("Connected to {}", state.hub_name)));
                        }
                    }
                    self.handle(response);
                }
                Job::Polled(Err(err)) => {
                    connected = false;
                    if err.removed {
                        self.tell(Event::Removed);
                        return;
                    }
                    if let Some(state) = self.snapshot() {
                        self.tell(Event::Status(format!(
                            "Can't reach {}: {err}",
                            state.hub_name
                        )));
                    }
                }
                Job::Stop => return,
            }
        }
    }

    fn send_report(&self, report: &Report) {
        if let Some(state) = self.snapshot()
            && let Err(err) = Client::new(&state, &*self.transport).report(report)
        {
            self.tell(Event::Status(format!(
                "Couldn't report to {}: {err}",
                state.hub_name
            )));
        }
    }

    fn update(&self, id: &str, update: &CommandUpdate) {
        if let Some(state) = self.snapshot() {
            let _: Result<(), AgentError> =
                Client::new(&state, &*self.transport).update(id, update);
        }
    }

    fn handle(&mut self, response: PollResponse) {
        let Some(state) = self.snapshot() else { return };
        for id in response.cancelled {
            self.asked.remove(&id);
            self.tell(Event::Withdrawn(id));
        }
        let policy = self
            .policy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        for signed in &response.commands {
            match commands::consider(&state, signed, &policy, now()) {
                Verdict::Ignore => {}
                Verdict::Refuse {
                    id,
                    state: outcome,
                    reason,
                } => {
                    self.update(&id, &plain(outcome, &reason));
                }
                Verdict::Run(request, approval) => self.carry_out(&request, approval),
                Verdict::Ask(request) | Verdict::AskApprover(request) => {
                    let id = request.command.id.clone();
                    if !self.asked.insert(id.clone()) {
                        continue;
                    }
                    self.update(&id, &plain(CommandState::Shown, ""));
                    let mut lines = commands::describe(&request.command, None);
                    if cfg!(target_os = "android") {
                        // describe() says permanently, which is right here.
                    } else if matches!(
                        request.command.action,
                        disktree_proto::Action::Delete { .. }
                    ) {
                        lines[0] = lines[0].replace("permanently delete", "move to the Trash");
                    }
                    self.tell(Event::Ask(Prompt {
                        id,
                        title: "A request from the Hub".into(),
                        lines,
                        minutes: commands::offered_minutes(&request.command),
                        kind: PromptKind::Own(Box::new(request)),
                    }));
                }
            }
        }
        for request in response.approvals {
            // Shown only if the Hub really signed it.
            let Ok(command) = request.command.open(&state.hub_key, Purpose::Command) else {
                continue;
            };
            if !self.asked.insert(command.id.clone()) {
                continue;
            }
            let mut lines = commands::describe(&command, Some(&request.device_name));
            if !request.note.trim().is_empty() {
                lines.push(request.note.trim().to_string());
            }
            self.tell(Event::Ask(Prompt {
                id: command.id.clone(),
                title: format!("{} asks for your approval", request.device_name),
                lines,
                minutes: commands::offered_minutes(&command),
                kind: PromptKind::ForOther {
                    request: Box::new(request),
                    command: Box::new(command),
                },
            }));
        }
    }

    fn carry_out(&self, request: &Request, approval: disktree_proto::Approval) {
        self.update(&request.command.id, &plain(CommandState::Running, ""));
        let update = commands::run(request, &*self.executor, approval);
        let deleted = match request.command.action {
            disktree_proto::Action::Delete { .. } => request
                .targets
                .iter()
                .filter(|t| t.symlink_metadata().is_err())
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        if let Some(state) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            state.mark_handled(&request.command.id);
        }
        self.save();
        self.update(&request.command.id, &update);
        self.tell(Event::Done {
            text: format!("{}: {}", request.command.action.summary(), update.detail),
            deleted,
        });
    }

    fn answer(&mut self, prompt: Prompt, choice: Choice) {
        self.asked.remove(&prompt.id);
        match prompt.kind {
            PromptKind::Own(request) => {
                let approval = {
                    let mut guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    let Some(state) = guard.as_mut() else { return };
                    commands::decide(state, &request, choice, ApprovedBy::Device, now())
                };
                self.save();
                match approval {
                    Some(approval) => self.carry_out(&request, approval),
                    None => self.update(
                        &request.command.id,
                        &plain(CommandState::Denied, "denied on the device"),
                    ),
                }
            }
            PromptKind::ForOther { request, command } => {
                let Some(state) = self.snapshot() else { return };
                let (allow, minutes) = match choice {
                    Choice::Deny => (false, 0),
                    Choice::Once => (true, 0),
                    Choice::For(m) => (true, m.min(commands::MAX_GRANT_MINUTES)),
                };
                let decision = Decision {
                    command: command.id.clone(),
                    command_hash: request.command.hash(),
                    device: request.device.clone(),
                    allow,
                    minutes,
                    time: now(),
                };
                if let Err(err) = Client::new(&state, &*self.transport).decide(&decision) {
                    self.tell(Event::Status(format!("Couldn't send the answer: {err}")));
                }
            }
        }
    }
}

fn plain(state: CommandState, detail: &str) -> CommandUpdate {
    CommandUpdate {
        state,
        detail: detail.to_string(),
        approval: None,
        freed: 0,
        failed: Vec::new(),
    }
}

/// Carries out approved requests with the app's own actions.
pub struct AppExecutor {
    /// The junk the app found last, for cleaning it on request.
    pub junk: Arc<Mutex<Vec<Junk>>>,
}

impl Executor for AppExecutor {
    fn delete(&self, path: &Path) -> Result<u64, String> {
        let bytes = crate::cleanup::dir_size(path);
        if cfg!(target_os = "android") {
            // As everywhere in the app on Android: there's no Trash.
            let (freed, errors) = crate::cleanup::delete(&[path.to_path_buf()]);
            match errors.into_iter().next() {
                Some(err) => Err(err),
                None => Ok(freed),
            }
        } else {
            crate::actions::delete(path).map(|()| bytes)
        }
    }

    fn uninstall(&self, package: &str) -> Result<String, String> {
        if !cfg!(target_os = "android") {
            return Err("only Android apps can be uninstalled this way".into());
        }
        crate::apps::uninstall(package);
        Ok("Android asked to confirm uninstalling".into())
    }

    fn clean(&self, kind: &str) -> Result<u64, String> {
        let junk = self
            .junk
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|j| j.kind.id() == kind)
            .cloned()
            .ok_or("nothing of that kind to clean any more")?;
        let (freed, errors) = crate::apps::delete_junk(&junk);
        if freed == 0
            && let Some(err) = errors.into_iter().next()
        {
            return Err(err);
        }
        Ok(freed)
    }
}

/// The Hub address and code typed into the connect dialog, or a pasted
/// link, as a pairing link.
pub fn link_from(text: &str, code: &str) -> Result<PairingLink, String> {
    let text = text.trim();
    if text.starts_with("disktree://") {
        return PairingLink::parse(text).ok_or_else(|| "That pairing link isn't complete.".into());
    }
    let hub = disktree_proto::pairing::normalize_hub(text)
        .ok_or("Type the Hub's address, or paste the pairing link.")?;
    let code = disktree_proto::pairing::format_code(code)
        .ok_or("A pairing code has 12 letters and digits.")?;
    Ok(PairingLink {
        hub,
        code,
        key: None,
    })
}

/// Android's apps and storage as the Hub stores them.
pub fn apps_report(report: &crate::apps::AppReport) -> disktree_proto::AppsReport {
    use crate::cleanup::JunkKind;
    let breakdown = disktree_core::disk_space(&crate::locations::default_path()).map(|space| {
        let b = report.breakdown(space);
        disktree_proto::Breakdown {
            total: b.total,
            free: b.free,
            apps: b.apps,
            files: b.files,
            system: b.system,
        }
    });
    disktree_proto::AppsReport {
        taken: report.now as i64,
        breakdown,
        apps: report
            .apps
            .iter()
            .map(|app| disktree_proto::AppEntry {
                package: app.package.clone(),
                label: app.label.clone(),
                code: app.code_bytes,
                data: app.data_bytes,
                cache: app.cache_bytes,
                last_used: app.last_opened.map(|t| t as i64),
                installed: app.installed_at.map(|t| t as i64),
                source: app
                    .installer_label
                    .clone()
                    .or_else(|| app.installer.clone())
                    .unwrap_or_default(),
                removable: crate::insights::suggestable(app),
            })
            .collect(),
        junk: report
            .junk
            .iter()
            .map(|junk| disktree_proto::JunkEntry {
                kind: junk.kind.id().to_string(),
                label: match junk.kind {
                    JunkKind::Trash => "Trash",
                    JunkKind::Installers => "Old app installers",
                    JunkKind::Thumbnails => "Thumbnail cache",
                    JunkKind::Leftovers => "Leftovers of removed apps",
                }
                .to_string(),
                bytes: junk.bytes,
                items: junk.paths.len() as u64,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_from_what_was_typed() {
        let link = link_from("hub.home.lan", "abcd efgh jkmn").unwrap();
        assert_eq!(link.hub, "https://hub.home.lan");
        assert_eq!(link.code, "ABCD-EFGH-JKMN");
        assert!(link.key.is_none());
        let pasted = link_from(
            " disktree://pair?hub=http%3A%2F%2Fx%3A9090&code=ABCDEFGHJKMN&key=AAAA-BBBB-CCCC-DDDD ",
            "",
        )
        .unwrap();
        assert_eq!(pasted.hub, "http://x:9090");
        assert_eq!(pasted.key.as_deref(), Some("AAAA-BBBB-CCCC-DDDD"));
        assert!(link_from("hub.lan", "short").is_err());
        assert!(link_from("", "ABCDEFGHJKMN").is_err());
    }
}
