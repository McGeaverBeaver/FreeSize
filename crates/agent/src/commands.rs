//! What a device does with a command from the Hub. Nothing runs unless
//!
//! - the Hub signed it, for this device, and it hasn't expired or run
//!   before;
//! - for deletes, every path is inside a folder this device scans, isn't
//!   reached through a symlink out of it, and isn't protected;
//! - someone allowed it: the person at the device, the designated approver
//!   from their own device (whose signature this device checks), or a grant
//!   one of them gave the same admin for the same kind of action.

use std::path::{Component, Path, PathBuf};

use disktree_proto::{
    Action, ActionKind, Approval, ApprovedBy, Command, CommandState, CommandUpdate, Decision,
    Grant, Purpose, Signed,
};

use crate::state::AgentState;

/// Longest grant a device accepts, whatever the Hub offers: 8 hours.
pub const MAX_GRANT_MINUTES: u32 = 8 * 60;

/// What this device allows.
#[derive(Clone, Debug, Default)]
pub struct Policy {
    /// Someone can approve on the device itself.
    pub attended: bool,
    pub actions: Vec<ActionKind>,
    /// Folders this device scans; deletes stay inside them.
    pub roots: Vec<PathBuf>,
    /// Files never to delete, nor any folder holding them, such as the
    /// agent's own state.
    pub keep: Vec<PathBuf>,
}

/// A command that checked out, with what it would touch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub command: Command,
    pub signed: Signed<Command>,
    /// For deletes: the full paths.
    pub targets: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Already handled, or not for this device: say nothing.
    Ignore,
    /// Not allowed; tell the Hub why.
    Refuse {
        id: String,
        state: CommandState,
        reason: String,
    },
    /// A grant covers it: run it.
    Run(Request, Approval),
    /// Ask the person at this device.
    Ask(Request),
    /// Ask the designated approver.
    AskApprover(Request),
}

/// Decides what to do with a command from the Hub.
pub fn consider(
    state: &AgentState,
    signed: &Signed<Command>,
    policy: &Policy,
    now: i64,
) -> Verdict {
    let refuse = |id: &str, state: CommandState, reason: String| Verdict::Refuse {
        id: id.to_string(),
        state,
        reason,
    };
    let Ok(command) = signed.open(&state.hub_key, Purpose::Command) else {
        // Not from our Hub: don't even answer it.
        return Verdict::Ignore;
    };
    if command.device != state.device || state.was_handled(&command.id) {
        return Verdict::Ignore;
    }
    if command.expires <= now {
        return refuse(
            &command.id,
            CommandState::Expired,
            "the request expired".into(),
        );
    }
    let kind = command.action.kind();
    if !policy.actions.contains(&kind) {
        return refuse(
            &command.id,
            CommandState::Denied,
            format!("this device doesn't {}", kind.label()),
        );
    }
    let targets = match &command.action {
        Action::Delete { root, paths, .. } => match delete_targets(root, paths, policy) {
            Ok(targets) => targets,
            Err(reason) => return refuse(&command.id, CommandState::Denied, reason),
        },
        _ => Vec::new(),
    };
    let request = Request {
        command,
        signed: signed.clone(),
        targets,
    };
    if let Some(grant) = state.grant_for(&request.command.by.subject, kind, now) {
        let approval = Approval {
            by: ApprovedBy::Grant {
                expires: grant.expires,
            },
            minutes: 0,
        };
        return Verdict::Run(request, approval);
    }
    if policy.attended {
        Verdict::Ask(request)
    } else if state.approver.is_some() {
        Verdict::AskApprover(request)
    } else {
        refuse(
            &request.command.id,
            CommandState::Denied,
            "nobody is at this device and no approver is set: run `disktree approver set \
             <fingerprint>` on it first"
                .into(),
        )
    }
}

/// The grant lengths to offer for a command: the Hub's, capped.
pub fn offered_minutes(command: &Command) -> Vec<u32> {
    let mut minutes: Vec<u32> = command
        .grant_minutes
        .iter()
        .copied()
        .filter(|&m| (1..=MAX_GRANT_MINUTES).contains(&m))
        .collect();
    minutes.sort_unstable();
    minutes.dedup();
    minutes.truncate(6);
    minutes
}

/// The answer to a prompt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Deny,
    Once,
    For(u32),
}

/// Records a decision on `request`: a grant if one was given. Returns the
/// approval to report, or `None` if it was denied.
pub fn decide(
    state: &mut AgentState,
    request: &Request,
    choice: Choice,
    by: ApprovedBy,
    now: i64,
) -> Option<Approval> {
    let minutes = match choice {
        Choice::Deny => {
            state.mark_handled(&request.command.id);
            return None;
        }
        Choice::Once => 0,
        Choice::For(m) => m.min(MAX_GRANT_MINUTES),
    };
    if minutes > 0 {
        state.add_grant(Grant {
            admin: request.command.by.subject.clone(),
            admin_name: request.command.by.name.clone(),
            kind: request.command.action.kind(),
            expires: now + i64::from(minutes) * 60,
        });
    }
    Some(Approval { by, minutes })
}

/// Checks a decision from the designated approver: signed by the approver
/// this device has set, about exactly this command.
pub fn check_decision(
    state: &AgentState,
    request: &Request,
    signed: &Signed<Decision>,
) -> Result<Choice, String> {
    let approver = state
        .approver
        .as_ref()
        .ok_or("no approver is set on this device")?;
    let decision = signed
        .open(&approver.key, Purpose::Decision)
        .map_err(|e| format!("the decision isn't from this device's approver ({e})"))?;
    if decision.command != request.command.id
        || decision.device != state.device
        || decision.command_hash != request.signed.hash()
    {
        return Err("the decision is about a different request".into());
    }
    Ok(match (decision.allow, decision.minutes) {
        (false, _) => Choice::Deny,
        (true, 0) => Choice::Once,
        (true, m) => Choice::For(m),
    })
}

/// The full paths of a delete, after checking each one.
pub fn delete_targets(
    root: &str,
    paths: &[String],
    policy: &Policy,
) -> Result<Vec<PathBuf>, String> {
    let root = PathBuf::from(root);
    let inside_scanned = policy
        .roots
        .iter()
        .any(|scanned| root == *scanned || root.starts_with(scanned));
    if !inside_scanned {
        return Err(format!(
            "{} isn't a folder this device scans",
            root.display()
        ));
    }
    let real_root = root
        .canonicalize()
        .map_err(|e| format!("{}: {e}", root.display()))?;
    if paths.is_empty() {
        return Err("nothing to delete".into());
    }
    let mut targets = Vec::with_capacity(paths.len());
    for relative in paths {
        let mut target = root.clone();
        for part in relative.split('/') {
            let safe = !part.is_empty()
                && part != "."
                && part != ".."
                && !part.contains(['\\', '\0'])
                && matches!(
                    Path::new(part).components().next(),
                    Some(Component::Normal(_))
                );
            if !safe {
                return Err(format!("{relative}: not a plain path below the folder"));
            }
            target.push(part);
        }
        // Follow symlinks in the folders above, but not the item itself: a
        // link inside the scanned folder mustn't lead a delete out of it.
        let parent = target
            .parent()
            .ok_or_else(|| format!("{relative}: no folder above it"))?
            .canonicalize()
            .map_err(|_| format!("{relative}: no longer there"))?;
        if !parent.starts_with(&real_root) {
            return Err(format!("{relative}: leads outside {}", root.display()));
        }
        let name = target
            .file_name()
            .ok_or_else(|| format!("{relative}: no name"))?;
        let real = parent.join(name);
        if std::fs::symlink_metadata(&real).is_err() {
            return Err(format!("{relative}: no longer there"));
        }
        if real == real_root {
            return Err("the scanned folder itself can't be deleted".into());
        }
        if let Some(why) = protected(&real, policy) {
            return Err(format!("{}: {why}", real.display()));
        }
        targets.push(real);
    }
    Ok(targets)
}

/// Why `path` must never be deleted, if it mustn't.
pub fn protected(path: &Path, policy: &Policy) -> Option<&'static str> {
    // The kept files themselves, and any folder holding one.
    if policy
        .keep
        .iter()
        .any(|keep| path == keep || keep.starts_with(path))
    {
        return Some("DiskTree's own files");
    }
    protected_text(
        &path.to_string_lossy(),
        cfg!(windows),
        cfg!(target_os = "macos"),
    )
}

/// The rules behind [`protected`], on a path's text, so every platform's
/// rules can be tested anywhere.
pub fn protected_text(path: &str, windows: bool, macos: bool) -> Option<&'static str> {
    let mut text = path.replace('\\', "/");
    if windows || macos {
        text = text.to_lowercase();
    }
    let text = text.trim_end_matches('/');
    let under = |dir: &str| text == dir || text.starts_with(&format!("{dir}/"));
    if text.is_empty() || (windows && text.len() <= 2 && text.ends_with(':')) {
        return Some("the top of a drive");
    }
    let name = text.rsplit('/').next().unwrap_or_default();
    if matches!(name, "docker.img" | "libvirt.img") {
        return Some("a virtual disk the system runs from");
    }
    if windows {
        let rest = text.split_once(':').map_or(text, |(_, rest)| rest);
        let system = [
            "/windows",
            "/program files",
            "/program files (x86)",
            "/programdata",
            "/system volume information",
            "/recovery",
            "/boot",
        ];
        if system
            .iter()
            .any(|dir| rest == *dir || rest.starts_with(&format!("{dir}/")))
        {
            return Some("a system folder");
        }
        if matches!(rest, "/pagefile.sys" | "/hiberfil.sys" | "/swapfile.sys") {
            return Some("managed by Windows; use its own settings");
        }
        return None;
    }
    let system: &[&str] = &[
        "/bin",
        "/boot",
        "/dev",
        "/etc",
        "/lib",
        "/lib32",
        "/lib64",
        "/libx32",
        "/proc",
        "/run",
        "/sbin",
        "/sys",
        "/usr",
        "/var/lib",
        "/snap",
        "/srv/.snapshots",
    ];
    if system.iter().any(|dir| under(dir)) {
        return Some("a system folder");
    }
    if macos
        && ["/system", "/library", "/private", "/applications/utilities"]
            .iter()
            .any(|d| under(d))
    {
        return Some("a system folder");
    }
    // Unraid: the flash drive is /boot; the system share holds docker.img,
    // libvirt.img and the VM settings.
    let parts: Vec<&str> = text.split('/').collect();
    if parts.len() >= 4 && parts[1] == "mnt" && parts[3] == "system" {
        return Some("Unraid's system share");
    }
    // Android's per-app folders, which Android manages.
    if text.contains("/android/data")
        || text.contains("/Android/data")
        || text.contains("/Android/obb")
    {
        return Some("an app's private folder");
    }
    None
}

/// Carries out commands on a device.
pub trait Executor {
    /// Deletes a file or folder; returns the bytes it held.
    fn delete(&self, path: &Path) -> Result<u64, String>;
    fn uninstall(&self, _package: &str) -> Result<String, String> {
        Err("this device can't uninstall apps".into())
    }
    fn clean(&self, _kind: &str) -> Result<u64, String> {
        Err("this device has no junk to clean".into())
    }
}

/// Deletes files permanently with the standard library. Symlinks are
/// removed, never followed.
pub struct FsExecutor;

fn size_of(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| size_of(&e.path())).sum())
        .unwrap_or(0)
}

impl Executor for FsExecutor {
    fn delete(&self, path: &Path) -> Result<u64, String> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        let bytes = size_of(path);
        if meta.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        }
        .map_err(|e| e.to_string())?;
        Ok(bytes)
    }
}

/// Carries out an allowed request and says how it went.
pub fn run(request: &Request, executor: &dyn Executor, approval: Approval) -> CommandUpdate {
    let mut update = CommandUpdate {
        state: CommandState::Done,
        detail: String::new(),
        approval: Some(approval),
        freed: 0,
        failed: Vec::new(),
    };
    match &request.command.action {
        Action::Delete { .. } => {
            let mut done = 0;
            for target in &request.targets {
                match executor.delete(target) {
                    Ok(bytes) => {
                        done += 1;
                        update.freed += bytes;
                    }
                    Err(err) => update.failed.push(format!("{}: {err}", target.display())),
                }
            }
            update.detail = format!(
                "deleted {done} of {}, freeing {}",
                request.targets.len(),
                disktree_core::format_size(update.freed)
            );
            if done == 0 {
                update.state = CommandState::Failed;
            }
        }
        Action::Uninstall { package, .. } => match executor.uninstall(package) {
            Ok(detail) => update.detail = detail,
            Err(err) => {
                update.state = CommandState::Failed;
                update.detail = err;
            }
        },
        Action::CleanJunk { kind, .. } => match executor.clean(kind) {
            Ok(bytes) => {
                update.freed = bytes;
                update.detail = format!("freed {}", disktree_core::format_size(bytes));
            }
            Err(err) => {
                update.state = CommandState::Failed;
                update.detail = err;
            }
        },
    }
    update
}

/// What a prompt says about a request, line by line, for showing on the
/// device or the approver's phone.
pub fn describe(command: &Command, device_name: Option<&str>) -> Vec<String> {
    let mut lines = Vec::new();
    let on = device_name.map(|d| format!(" on {d}")).unwrap_or_default();
    match &command.action {
        Action::Delete { root, paths, bytes } => {
            lines.push(format!(
                "{} asks to permanently delete {} ({}){on}:",
                command.by.name,
                match paths.len() {
                    1 => "1 item".to_string(),
                    n => format!("{n} items"),
                },
                disktree_core::format_size(*bytes)
            ));
            lines.extend(paths.iter().take(50).map(|p| format!("{root}/{p}")));
            if paths.len() > 50 {
                lines.push(format!("… and {} more", paths.len() - 50));
            }
        }
        Action::Uninstall { label, package } => {
            lines.push(format!(
                "{} asks to uninstall {label} ({package}){on}.",
                command.by.name
            ));
            lines.push("Android asks you to confirm as well.".into());
        }
        Action::CleanJunk { label, bytes, .. } => lines.push(format!(
            "{} asks to clean up {label} ({}){on}.",
            command.by.name,
            disktree_core::format_size(*bytes)
        )),
    }
    if !command.reason.trim().is_empty() {
        lines.push(format!("Reason: {}", command.reason.trim()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use std::fs;

    use disktree_proto::{Admin, KeyPair};

    use super::*;
    use crate::state::Approver;

    struct Setup {
        dir: PathBuf,
        hub: KeyPair,
        state: AgentState,
        policy: Policy,
    }

    impl Drop for Setup {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn setup(name: &str, attended: bool) -> Setup {
        let dir =
            std::env::temp_dir().join(format!("disktree-agent-cmd-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("share/movies")).unwrap();
        fs::write(dir.join("share/movies/a.mkv"), vec![1u8; 5000]).unwrap();
        fs::write(dir.join("share/b.iso"), vec![1u8; 3000]).unwrap();
        fs::create_dir_all(dir.join("outside")).unwrap();
        fs::write(dir.join("outside/precious.txt"), b"keep").unwrap();
        let hub = KeyPair::from_seed([1; 32]);
        let device = KeyPair::from_seed([2; 32]);
        let state = AgentState::new(
            "http://hub".into(),
            "Hub".into(),
            hub.public(),
            "D1".into(),
            &device,
        );
        let policy = Policy {
            attended,
            actions: vec![ActionKind::Delete],
            roots: vec![dir.join("share")],
            keep: vec![],
        };
        Setup {
            dir,
            hub,
            state,
            policy,
        }
    }

    fn command(setup: &Setup, id: &str, paths: &[&str]) -> Signed<Command> {
        let command = Command {
            id: id.into(),
            device: "D1".into(),
            action: Action::Delete {
                root: setup.dir.join("share").to_string_lossy().into_owned(),
                paths: paths.iter().map(|p| p.to_string()).collect(),
                bytes: 8000,
            },
            by: Admin {
                subject: "admin-1".into(),
                name: "Robert".into(),
            },
            reason: "space".into(),
            created: 100,
            expires: 10_000,
            grant_minutes: vec![10, 30, 9999],
        };
        Signed::sign(&command, &setup.hub, Purpose::Command)
    }

    #[test]
    fn attended_devices_ask_and_grants_skip_asking() {
        let mut s = setup("ask", true);
        let signed = command(&s, "C1", &["movies/a.mkv", "b.iso"]);
        let Verdict::Ask(request) = consider(&s.state, &signed, &s.policy, 500) else {
            panic!("should ask");
        };
        assert_eq!(request.targets.len(), 2);
        assert_eq!(offered_minutes(&request.command), [10, 30]);
        let approval = decide(
            &mut s.state,
            &request,
            Choice::For(10),
            ApprovedBy::Device,
            500,
        )
        .unwrap();
        assert_eq!(approval.minutes, 10);
        let update = run(&request, &FsExecutor, approval);
        assert_eq!(update.state, CommandState::Done);
        assert_eq!(update.freed, 8000);
        assert!(!s.dir.join("share/b.iso").exists());
        s.state.mark_handled("C1");
        // Already handled: ignored.
        assert_eq!(consider(&s.state, &signed, &s.policy, 501), Verdict::Ignore);

        // The grant covers the same admin and kind for 10 minutes.
        fs::write(s.dir.join("share/c.bin"), b"x").unwrap();
        let next = command(&s, "C2", &["c.bin"]);
        assert!(matches!(
            consider(&s.state, &next, &s.policy, 600),
            Verdict::Run(..)
        ));
        assert!(matches!(
            consider(&s.state, &next, &s.policy, 500 + 601),
            Verdict::Ask(..)
        ));

        let lines = describe(&request.command, Some("Tower"));
        assert!(lines[0].starts_with("Robert asks to permanently delete 2 items"));
        assert_eq!(lines.last().unwrap(), "Reason: space");
    }

    #[test]
    fn forged_or_unsafe_commands_are_refused() {
        let s = setup("refuse", true);
        // Signed by someone other than the Hub: ignored.
        let mut forged = command(&s, "C1", &["b.iso"]);
        let other = KeyPair::from_seed([9; 32]);
        let real: Command = forged.peek().unwrap();
        forged = Signed::sign(&real, &other, Purpose::Command);
        assert_eq!(consider(&s.state, &forged, &s.policy, 500), Verdict::Ignore);

        let refused =
            |paths: &[&str]| match consider(&s.state, &command(&s, "C2", paths), &s.policy, 500) {
                Verdict::Refuse { reason, .. } => reason,
                other => panic!("expected a refusal, got {other:?}"),
            };
        assert!(refused(&["../outside/precious.txt"]).contains("not a plain path"));
        assert!(refused(&["/etc/passwd"]).contains("not a plain path"));
        assert!(refused(&["missing.bin"]).contains("no longer there"));
        assert!(refused(&[]).contains("nothing to delete"));
        // A symlink out of the scanned folder doesn't lead a delete out.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(s.dir.join("outside"), s.dir.join("share/link")).unwrap();
            assert!(refused(&["link/precious.txt"]).contains("leads outside"));
            // The link itself may go; it's inside.
            assert!(matches!(
                consider(&s.state, &command(&s, "C3", &["link"]), &s.policy, 500),
                Verdict::Ask(..)
            ));
        }
        // Expired.
        assert!(matches!(
            consider(&s.state, &command(&s, "C4", &["b.iso"]), &s.policy, 20_000),
            Verdict::Refuse {
                state: CommandState::Expired,
                ..
            }
        ));
        // A root this device doesn't scan.
        let mut other_root = command(&s, "C5", &["precious.txt"]).peek().unwrap();
        other_root.action = Action::Delete {
            root: s.dir.join("outside").to_string_lossy().into_owned(),
            paths: vec!["precious.txt".into()],
            bytes: 4,
        };
        let signed = Signed::sign(&other_root, &s.hub, Purpose::Command);
        assert!(matches!(
            consider(&s.state, &signed, &s.policy, 500),
            Verdict::Refuse { .. }
        ));
        assert!(s.dir.join("outside/precious.txt").exists());
    }

    #[test]
    fn unattended_devices_need_their_approver() {
        let mut s = setup("approver", false);
        let signed = command(&s, "C1", &["b.iso"]);
        assert!(matches!(
            consider(&s.state, &signed, &s.policy, 500),
            Verdict::Refuse { reason, .. } if reason.contains("approver set")
        ));
        let phone = KeyPair::from_seed([5; 32]);
        s.state.approver = Some(Approver {
            device: "P1".into(),
            name: "Phone".into(),
            key: phone.public(),
        });
        let Verdict::AskApprover(request) = consider(&s.state, &signed, &s.policy, 500) else {
            panic!("should ask the approver");
        };
        let decision = |allow, minutes, hash: String| Decision {
            command: "C1".into(),
            command_hash: hash,
            device: "D1".into(),
            allow,
            minutes,
            time: 500,
        };
        let good = Signed::sign(
            &decision(true, 30, signed.hash()),
            &phone,
            Purpose::Decision,
        );
        assert_eq!(
            check_decision(&s.state, &request, &good),
            Ok(Choice::For(30))
        );
        // Another device's signature doesn't count, nor a decision about
        // something else.
        let stranger = KeyPair::from_seed([6; 32]);
        let forged = Signed::sign(
            &decision(true, 30, signed.hash()),
            &stranger,
            Purpose::Decision,
        );
        assert!(check_decision(&s.state, &request, &forged).is_err());
        let other = Signed::sign(&decision(true, 30, "abc".into()), &phone, Purpose::Decision);
        assert!(check_decision(&s.state, &request, &other).is_err());
        let no = Signed::sign(
            &decision(false, 0, signed.hash()),
            &phone,
            Purpose::Decision,
        );
        assert_eq!(check_decision(&s.state, &request, &no), Ok(Choice::Deny));
    }

    #[test]
    fn protected_places() {
        let linux = |p: &str| protected_text(p, false, false);
        assert!(linux("/").is_some());
        assert!(linux("/etc/ssh").is_some());
        assert!(linux("/boot/config/plugins").is_some());
        assert!(linux("/mnt/user/system/docker").is_some());
        assert!(linux("/mnt/disk3/system").is_some());
        assert!(linux("/mnt/cache/appdata/plex/docker.img").is_some());
        assert!(linux("/mnt/user/media/movies").is_none());
        assert!(linux("/home/robert/Downloads/x.iso").is_none());
        assert!(linux("/storage/emulated/0/Android/data/com.x").is_some());
        let windows = |p: &str| protected_text(p, true, false);
        assert!(windows("C:\\").is_some());
        assert!(windows("C:\\Windows\\System32").is_some());
        assert!(windows("c:\\program files\\Steam").is_some());
        assert!(windows("C:\\hiberfil.sys").is_some());
        assert!(windows("D:\\Games\\old").is_none());
        assert!(windows("C:\\Users\\Robert\\Downloads\\big.zip").is_none());
        let mac = |p: &str| protected_text(p, false, true);
        assert!(mac("/System/Library").is_some());
        assert!(mac("/Users/robert/Movies/a.mov").is_none());
    }
}
