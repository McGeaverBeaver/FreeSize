//! What a paired device remembers: the Hub, its key, the device's own key,
//! the designated approver and active grants. Kept in one JSON file only
//! the device's user can read.

use std::io::Write;
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use freesize_proto::{ActionKind, Grant, KeyPair, PublicKey};
use serde::{Deserialize, Serialize};

use crate::transport::Trust;

/// Command ids remembered so a command is never carried out twice.
const REMEMBERED_COMMANDS: usize = 500;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentState {
    /// The Hub's address.
    pub hub: String,
    pub hub_name: String,
    /// The Hub's key, learned (and checked) at pairing. Commands must carry
    /// its signature.
    pub hub_key: PublicKey,
    /// The Hub's own certificate authority (PEM), trusted for `hub` only;
    /// none when the Hub's certificate is checked against the system's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ca: Option<String>,
    /// This device's id at the Hub.
    pub device: String,
    /// The name it paired under.
    #[serde(default)]
    pub name: String,
    /// This device's private key, as its base64 seed.
    key: String,
    /// The approver set on this device, for a device nobody sits at.
    #[serde(default)]
    pub approver: Option<Approver>,
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// Ids of commands already handled.
    #[serde(default)]
    pub handled: Vec<String>,
}

/// The device whose decisions this device accepts.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approver {
    pub device: String,
    pub name: String,
    pub key: PublicKey,
}

impl AgentState {
    pub fn new(
        hub: String,
        hub_name: String,
        hub_key: PublicKey,
        device: String,
        key: &KeyPair,
    ) -> AgentState {
        AgentState {
            hub,
            hub_name,
            hub_key,
            ca: None,
            device,
            name: String::new(),
            key: STANDARD.encode(key.seed()),
            approver: None,
            grants: Vec::new(),
            handled: Vec::new(),
        }
    }

    fn seed(&self) -> Option<[u8; 32]> {
        STANDARD
            .decode(&self.key)
            .ok()
            .and_then(|b| b.try_into().ok())
    }

    pub fn key(&self) -> KeyPair {
        // `load` refuses a file whose key doesn't decode, and `new` writes
        // a fresh one, so this never falls back to a known key.
        KeyPair::from_seed(self.seed().expect("the device key was checked when loaded"))
    }

    pub fn fingerprint(&self) -> String {
        self.key().public().fingerprint()
    }

    /// What requests to the Hub trust.
    pub fn trust(&self) -> Trust {
        match &self.ca {
            Some(ca) => Trust::Authority(ca.clone()),
            None => Trust::System,
        }
    }

    pub fn load(path: &Path) -> Result<Option<AgentState>, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                let state: AgentState =
                    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                if state.seed().is_none() {
                    return Err(format!(
                        "{}: the device key in it is damaged; pair again",
                        path.display()
                    ));
                }
                Ok(Some(state))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(format!("{}: {err}", path.display())),
        }
    }

    /// Saves atomically, readable only by this user.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let fail = |e: std::io::Error| format!("{}: {e}", path.display());
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(fail)?;
        }
        let temp = path.with_extension("json.tmp");
        let _ = std::fs::remove_file(&temp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp).map_err(fail)?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(fail)?;
        file.sync_all().map_err(fail)?;
        drop(file);
        std::fs::rename(&temp, path).map_err(fail)
    }

    /// Grants still running at `now`, dropping expired ones.
    pub fn active_grants(&mut self, now: i64) -> &[Grant] {
        self.grants.retain(|g| g.expires > now);
        &self.grants
    }

    /// The grant letting `admin` do `kind` at `now`, if any.
    pub fn grant_for(&self, admin: &str, kind: ActionKind, now: i64) -> Option<&Grant> {
        self.grants
            .iter()
            .find(|g| g.admin == admin && g.kind == kind && g.expires > now)
    }

    pub fn add_grant(&mut self, grant: Grant) {
        self.grants
            .retain(|g| !(g.admin == grant.admin && g.kind == grant.kind));
        self.grants.push(grant);
    }

    pub fn revoke(&mut self, admin: Option<&str>, kind: Option<ActionKind>) -> usize {
        let before = self.grants.len();
        self.grants
            .retain(|g| !(admin.is_none_or(|a| g.admin == a) && kind.is_none_or(|k| g.kind == k)));
        before - self.grants.len()
    }

    pub fn was_handled(&self, command: &str) -> bool {
        self.handled.iter().any(|c| c == command)
    }

    pub fn mark_handled(&mut self, command: &str) {
        if !self.was_handled(command) {
            self.handled.push(command.to_string());
        }
        let extra = self.handled.len().saturating_sub(REMEMBERED_COMMANDS);
        self.handled.drain(..extra);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_privately() {
        let dir = std::env::temp_dir().join(format!("freesize-agent-state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("agent.json");
        assert!(AgentState::load(&path).unwrap().is_none());
        let key = KeyPair::generate();
        let hub = KeyPair::generate();
        let mut state = AgentState::new(
            "https://hub".into(),
            "Hub".into(),
            hub.public(),
            "D1".into(),
            &key,
        );
        state.add_grant(Grant {
            admin: "a".into(),
            admin_name: "Ann".into(),
            kind: ActionKind::Delete,
            expires: 100,
        });
        state.save(&path).unwrap();
        let back = AgentState::load(&path).unwrap().unwrap();
        assert_eq!(back.key().public(), key.public());
        assert_eq!(back.hub_key, hub.public());
        assert_eq!(back.grants.len(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // A damaged key is refused rather than replaced by a known one.
        let text = std::fs::read_to_string(&path).unwrap();
        let damaged = text.replace(&back.key, "bm90IGEga2V5");
        assert_ne!(text, damaged);
        std::fs::write(&path, damaged).unwrap();
        let err = AgentState::load(&path).unwrap_err();
        assert!(err.contains("damaged"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn grants_expire_and_revoke() {
        let key = KeyPair::generate();
        let mut state = AgentState::new("h".into(), "H".into(), key.public(), "D".into(), &key);
        let grant = |admin: &str, kind, expires| Grant {
            admin: admin.into(),
            admin_name: admin.into(),
            kind,
            expires,
        };
        state.add_grant(grant("a", ActionKind::Delete, 100));
        state.add_grant(grant("a", ActionKind::Delete, 200));
        state.add_grant(grant("b", ActionKind::Uninstall, 50));
        assert_eq!(state.grants.len(), 2);
        assert!(state.grant_for("a", ActionKind::Delete, 150).is_some());
        assert!(state.grant_for("a", ActionKind::Uninstall, 10).is_none());
        assert!(state.grant_for("b", ActionKind::Uninstall, 60).is_none());
        assert_eq!(state.active_grants(60).len(), 1);
        assert_eq!(state.revoke(Some("a"), None), 1);
        for i in 0..600 {
            state.mark_handled(&format!("c{i}"));
        }
        assert_eq!(state.handled.len(), REMEMBERED_COMMANDS);
        assert!(state.was_handled("c599"));
        assert!(!state.was_handled("c0"));
    }
}
