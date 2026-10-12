//! Talking to the Hub as a paired device: every request signed with the
//! device's key.

use disktree_proto::signing::{
    DEVICE_HEADER, NONCE_HEADER, SIGNATURE_HEADER, TIME_HEADER, sign_request,
};
use disktree_proto::{
    CommandUpdate, Decision, DeviceInfo, DeviceKey, ErrorBody, Hello, KeyPair, PROTOCOL,
    PairRequest, PairResponse, PairingLink, PollRequest, PollResponse, Purpose, Report, Signed,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::now;
use crate::state::AgentState;
use crate::transport::Transport;

/// Why something sent to the Hub didn't work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentError {
    pub message: String,
    /// The Hub answered with this status; 0 if it couldn't be reached.
    pub status: u16,
    /// The Hub removed this device: forget the pairing.
    pub removed: bool,
}

impl AgentError {
    fn new(message: impl Into<String>) -> AgentError {
        AgentError {
            message: message.into(),
            status: 0,
            removed: false,
        }
    }

    /// Whether trying again later may work (the Hub wasn't reachable or had
    /// a problem), as opposed to the Hub refusing.
    pub fn temporary(&self) -> bool {
        self.status == 0 || self.status >= 500
    }
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for AgentError {}

pub type AgentResult<T> = Result<T, AgentError>;

/// Seconds a normal request may take; a poll waits longer.
const TIMEOUT: u64 = 30;

fn decode<T: DeserializeOwned>(status: u16, body: &[u8]) -> AgentResult<T> {
    if (200..300).contains(&status) {
        return serde_json::from_slice(body)
            .map_err(|e| AgentError::new(format!("the Hub's answer: {e}")));
    }
    let error: Option<ErrorBody> = serde_json::from_slice(body).ok();
    Err(AgentError {
        message: error
            .as_ref()
            .map_or_else(|| format!("the Hub answered {status}"), |e| e.error.clone()),
        status,
        removed: error.is_some_and(|e| e.removed),
    })
}

/// The Hub at `hub` (an address like `https://hub.home.lan`).
pub fn hello(transport: &dyn Transport, hub: &str) -> AgentResult<Hello> {
    let (status, body) = transport
        .request(
            "GET",
            &format!("{hub}/api/v1/hello"),
            &[("Accept", "application/json".into())],
            None,
            TIMEOUT,
        )
        .map_err(|e| AgentError::new(format!("can't reach {hub}: {e}")))?;
    let hello: Hello = decode(status, &body)?;
    if hello.protocol != PROTOCOL {
        return Err(AgentError::new(format!(
            "this Hub speaks a different version (protocol {}); update DiskTree",
            hello.protocol
        )));
    }
    Ok(hello)
}

/// Pairs with a Hub using a link from its dashboard, checking the Hub has
/// the key the link names. Without a key in the link, `confirm` is shown the
/// Hub's fingerprint and decides.
pub fn pair(
    transport: &dyn Transport,
    link: &PairingLink,
    info: DeviceInfo,
    confirm: &mut dyn FnMut(&Hello) -> bool,
) -> AgentResult<AgentState> {
    let hello = hello(transport, &link.hub)?;
    match &link.key {
        Some(expected) if hello.key.fingerprint() != expected.to_uppercase() => {
            return Err(AgentError::new(format!(
                "the Hub at {} has the key {}, not {expected} as the pairing link says. \
                 Something between this device and the Hub may be pretending to be it.",
                link.hub,
                hello.key.fingerprint()
            )));
        }
        Some(_) => {}
        None => {
            if !confirm(&hello) {
                return Err(AgentError::new("pairing cancelled"));
            }
        }
    }
    let key = KeyPair::generate();
    let request = Signed::sign(
        &PairRequest {
            code: link.code.clone(),
            key: key.public(),
            info,
            time: now(),
        },
        &key,
        Purpose::PairRequest,
    );
    let body = serde_json::to_vec(&request).map_err(|e| AgentError::new(e.to_string()))?;
    let (status, answer) = transport
        .request(
            "POST",
            &format!("{}/api/v1/pair", link.hub),
            &[("Content-Type", "application/json".into())],
            Some(&body),
            TIMEOUT,
        )
        .map_err(|e| AgentError::new(format!("can't reach {}: {e}", link.hub)))?;
    let signed: Signed<PairResponse> = decode(status, &answer)?;
    let response = signed
        .open(&hello.key, Purpose::PairResponse)
        .map_err(|e| AgentError::new(format!("the Hub's answer: {e}")))?;
    if response.device_key != key.public() || response.hub.key != hello.key {
        return Err(AgentError::new(
            "the Hub's answer was meant for another device",
        ));
    }
    Ok(AgentState::new(
        link.hub.clone(),
        hello.name,
        hello.key,
        response.device,
        &key,
    ))
}

/// A paired device's connection to its Hub.
pub struct Client<'a> {
    pub state: &'a AgentState,
    transport: &'a dyn Transport,
}

impl<'a> Client<'a> {
    pub fn new(state: &'a AgentState, transport: &'a dyn Transport) -> Client<'a> {
        Client { state, transport }
    }

    /// A signed request to `path` (e.g. `/api/v1/report`). The signature
    /// covers the path below the Hub's address, so a Hub behind a reverse
    /// proxy under a sub-path still checks it.
    fn signed<T: DeserializeOwned>(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
        timeout: u64,
    ) -> AgentResult<T> {
        let key = self.state.key();
        let bytes = body.unwrap_or_default();
        let signature = sign_request(&key, &self.state.device, method, path, &bytes, now());
        let mut headers = vec![
            (DEVICE_HEADER, signature.device),
            (TIME_HEADER, signature.time.to_string()),
            (NONCE_HEADER, signature.nonce),
            (SIGNATURE_HEADER, signature.signature),
            ("Accept", "application/json".to_string()),
        ];
        if method != "GET" {
            headers.push(("Content-Type", "application/json".to_string()));
        }
        let url = format!("{}{path}", self.state.hub);
        let (status, answer) = self
            .transport
            .request(
                method,
                &url,
                &headers,
                (method != "GET").then_some(bytes.as_slice()),
                timeout,
            )
            .map_err(|e| AgentError::new(format!("can't reach the Hub: {e}")))?;
        decode(status, &answer)
    }

    fn post<B: Serialize, T: DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
        timeout: u64,
    ) -> AgentResult<T> {
        let bytes = serde_json::to_vec(body).map_err(|e| AgentError::new(e.to_string()))?;
        self.signed("POST", path, Some(bytes), timeout)
    }

    pub fn report(&self, report: &Report) -> AgentResult<()> {
        self.post::<_, serde_json::Value>("/api/v1/report", report, 120)?;
        Ok(())
    }

    /// Waits up to `wait` seconds for commands, approval requests or
    /// decisions.
    pub fn poll(&self, wait: u32) -> AgentResult<PollResponse> {
        self.post(
            "/api/v1/poll",
            &PollRequest { wait },
            u64::from(wait) + TIMEOUT,
        )
    }

    pub fn update(&self, command: &str, update: &CommandUpdate) -> AgentResult<()> {
        self.post::<_, serde_json::Value>(&format!("/api/v1/commands/{command}"), update, TIMEOUT)?;
        Ok(())
    }

    /// Sends a decision made on this device for another device's command.
    pub fn decide(&self, decision: &Decision) -> AgentResult<()> {
        let signed = Signed::sign(decision, &self.state.key(), Purpose::Decision);
        self.post::<_, serde_json::Value>("/api/v1/decisions", &signed, TIMEOUT)?;
        Ok(())
    }

    /// Another paired device's key by its fingerprint, checked against it.
    pub fn device_key(&self, fingerprint: &str) -> AgentResult<DeviceKey> {
        let fingerprint = fingerprint.trim().to_uppercase();
        let found: DeviceKey = self.signed(
            "GET",
            &format!(
                "/api/v1/devices/{}",
                disktree_proto::pairing::encode(&fingerprint)
            ),
            None,
            TIMEOUT,
        )?;
        // The Hub could answer with any key; only one that matches the
        // fingerprint the person typed counts.
        if found.key.fingerprint() != fingerprint {
            return Err(AgentError::new(
                "the Hub answered with a key that doesn't match that fingerprint",
            ));
        }
        Ok(found)
    }
}
