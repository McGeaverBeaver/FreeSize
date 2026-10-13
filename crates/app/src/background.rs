//! The phone in the background (0.14): what the Java `HubJob` runs every
//! 15 minutes while the app is closed. It sends what the app couldn't
//! (reports kept in `hub-pending/`), reports the phone's disks, quarantine
//! and Nextcloud folder, every six hours its apps and junk, and asks the Hub
//! for news. Anything that needs the person comes back to Java as a
//! notification that opens the app: nothing is deleted or cleaned from here,
//! not even under a grant; the app does that once it's open, with the paths
//! shown.

use std::path::Path;
use std::time::{Duration, SystemTime};

use freesize_agent::commands::{self, Policy, Verdict};
use freesize_agent::{AgentState, Client, now};
use freesize_proto::{CommandState, CommandUpdate, PollResponse, Purpose, Report};
use jni::objects::{JClass, JObject, JString};
use jni::{Env, EnvUnowned};
use serde::Serialize;

use crate::android;

/// How often the slow reports (every app measured, the junk rules run) go
/// out from the background.
const SLOW_REPORTS_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// How long one poll waits for news: the job has a slot of about ten
/// minutes, but shouldn't use it.
const POLL_WAIT: u32 = 5;

/// What Java shows.
#[derive(Debug, Default, Serialize)]
struct Answer {
    notices: Vec<Notice>,
    /// Tags of notices to take down: requests the Hub withdrew.
    cancel: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Notice {
    /// The request's id, which names the notification: a request shown
    /// again replaces its notice instead of adding one.
    tag: String,
    title: String,
    text: String,
}

/// `HubJob.nativeRun(Context, String)`: one round, answered as JSON (an
/// empty string for nothing to show).
#[unsafe(no_mangle)]
pub extern "system" fn Java_io_github_mcgeaverbeaver_freesize_HubJob_nativeRun<'caller>(
    mut unowned_env: EnvUnowned<'caller>,
    _class: JClass<'caller>,
    context: JObject<'caller>,
    data_dir: JString<'caller>,
) -> JString<'caller> {
    unowned_env
        .with_env(
            |env: &mut Env<'caller>| -> jni::errors::Result<JString<'caller>> {
                android::init_background(env, &context)?;
                let data_dir = data_dir.try_to_string(env)?;
                let answer = round(Path::new(&data_dir));
                let json = if answer.notices.is_empty() && answer.cancel.is_empty() {
                    String::new()
                } else {
                    serde_json::to_string(&answer).unwrap_or_default()
                };
                env.new_string(json)
            },
        )
        .resolve::<jni::errors::LogErrorAndDefault>()
}

/// One background round, from the app's data folder.
fn round(data_dir: &Path) -> Answer {
    let mut answer = Answer::default();
    if crate::hub::worker_active() {
        // The app is open and talking to the Hub itself.
        return answer;
    }
    let hub_path = data_dir.join("hub.json");
    let state = match AgentState::load(&hub_path) {
        Ok(Some(state)) => state,
        Ok(None) => return answer,
        Err(err) => {
            android::log(&format!("background: couldn't read the pairing: {err}"));
            return answer;
        }
    };
    let transport = android::JavaTransport;
    let client = Client::new(&state, &transport);

    // Reports: the kept ones first, then fresh.
    let sent = crate::hub::send_pending(&client, &hub_path);
    if sent > 0 {
        android::log(&format!("background: sent {sent} kept report(s)"));
    }
    let report = fresh_report(&state, data_dir);
    if let Err(err) = client.report(&report) {
        android::log(&format!("background: couldn't report: {err}"));
        crate::hub::save_pending(&hub_path, &report);
        // The Hub is out of reach; polling would fail the same way.
        return answer;
    }

    // News.
    let policy = crate::hub::policy(&hub_path);
    match client.poll(POLL_WAIT) {
        Ok(response) => notices(&hub_path, state, &transport, &policy, response, &mut answer),
        Err(err) => android::log(&format!("background: couldn't poll: {err}")),
    }
    answer
}

/// The phone's state now: its info and disks, the quarantine, the
/// Nextcloud folder, and every six hours the apps and the junk.
fn fresh_report(state: &AgentState, data_dir: &Path) -> Report {
    let places: Vec<(String, std::path::PathBuf)> = crate::locations::list()
        .into_iter()
        .map(|l| (l.label, l.path))
        .collect();
    let mut report = Report {
        info: Some(crate::hub::device_info(
            &android::device_name(),
            &android::os_name(),
            Some(state),
        )),
        disks: Some(freesize_agent::report::disks(&places)),
        quarantine: Some(crate::quarantine::list()),
        ..Report::default()
    };
    let accounts = freesize_agent::nextcloud::android_accounts(&crate::locations::default_path());
    if !accounts.is_empty() {
        report.sync = Some(freesize_agent::nextcloud::report(&accounts, now()));
    }
    let marker = data_dir.join("hub-pending").join("slow-reports-sent");
    let due = std::fs::metadata(&marker)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_none_or(|since| since >= SLOW_REPORTS_EVERY);
    if due {
        match crate::apps::collect() {
            Ok(apps) => report.apps = Some(crate::hub::apps_report(&apps)),
            Err(err) => android::log(&format!("background: couldn't measure the apps: {err}")),
        }
        {
            use freesize_core::junk::{Env, Platform, find, running_programs};
            let findings = find(
                Platform::current(),
                &Env::system(),
                &running_programs(),
                freesize_core::junk::now(),
            );
            report.junk = Some(freesize_agent::junk::report(&findings, now()));
        }
        if let Some(dir) = marker.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(&marker, b"");
    }
    report
}

/// Turns what the Hub sent into notices. Requests for this phone are
/// marked shown; what a grant would run waits for the app, and is said so.
fn notices(
    hub_path: &Path,
    mut state: AgentState,
    transport: &dyn freesize_agent::Transport,
    policy: &Policy,
    response: PollResponse,
    answer: &mut Answer,
) {
    if let Some(signed) = &response.move_to {
        match freesize_agent::move_to(transport, &mut state, signed) {
            Ok(true) => {
                let _ = state.save(hub_path);
            }
            Ok(false) => {}
            Err(err) => android::log(&format!("background: the Hub asked to move: {err}")),
        }
    }
    let client = Client::new(&state, transport);
    answer.cancel = response.cancelled.clone();
    let open = "Open FreeSize to see every path and allow or deny it.";
    for signed in &response.commands {
        match commands::consider(&state, signed, policy, now()) {
            Verdict::Ignore => {}
            Verdict::Refuse {
                id,
                state: outcome,
                reason,
            } => {
                let _ = client.update(&id, &update(outcome, &reason));
            }
            Verdict::Run(request, _) => {
                // Approved for a while, but nothing runs with the app closed.
                answer.notices.push(Notice {
                    tag: request.command.id.clone(),
                    title: "A request from the Hub is waiting".into(),
                    text: format!(
                        "{} It runs when you open FreeSize.",
                        first_line(&request.command, None)
                    ),
                });
            }
            Verdict::Ask(request) | Verdict::AskApprover(request) => {
                let id = request.command.id.clone();
                let _ = client.update(&id, &update(CommandState::Shown, ""));
                answer.notices.push(Notice {
                    tag: id,
                    title: "A request from the Hub".into(),
                    text: format!("{} {open}", first_line(&request.command, None)),
                });
            }
        }
    }
    for request in response.approvals {
        let Ok(command) = request.command.open(&state.hub_key, Purpose::Command) else {
            continue;
        };
        answer.notices.push(Notice {
            tag: command.id.clone(),
            title: format!("{} asks for your approval", request.device_name),
            text: format!(
                "{} {open}",
                first_line(&command, Some(&request.device_name))
            ),
        });
    }
}

/// The request in one line, as the app's prompt starts: a delete here
/// goes to the quarantine.
fn first_line(command: &freesize_proto::Command, device: Option<&str>) -> String {
    let mut line = commands::describe(command, device)
        .into_iter()
        .next()
        .unwrap_or_else(|| command.action.summary());
    if device.is_none() && matches!(command.action, freesize_proto::Action::Delete { .. }) {
        line = line.replace("permanently delete", "move to the quarantine");
    }
    line
}

fn update(state: CommandState, detail: &str) -> CommandUpdate {
    CommandUpdate {
        state,
        detail: detail.to_string(),
        approval: None,
        freed: 0,
        failed: Vec::new(),
    }
}
