//! Talking to the Hub as a paired device: every request signed with the
//! device's key.

use freesize_proto::signing::{
    DEVICE_HEADER, NONCE_HEADER, SIGNATURE_HEADER, TIME_HEADER, sign_request,
};
use freesize_proto::{
    CommandUpdate, Decision, DeviceInfo, DeviceKey, ErrorBody, Hello, HubAddress, KeyPair,
    PROTOCOL, PairRequest, PairResponse, PairingLink, PollRequest, PollResponse, Purpose, Report,
    Signed,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::now;
use crate::state::AgentState;
use crate::transport::{Transport, Trust};

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
pub fn hello(transport: &dyn Transport, hub: &str, trust: &Trust) -> AgentResult<Hello> {
    let (status, body) = transport
        .request(
            "GET",
            &format!("{hub}/api/v1/hello"),
            &[("Accept", "application/json".into())],
            None,
            TIMEOUT,
            trust,
        )
        .map_err(|e| AgentError::new(format!("can't reach {hub}: {e}")))?;
    let hello: Hello = decode(status, &body)?;
    if hello.protocol != PROTOCOL {
        return Err(AgentError::new(format!(
            "this Hub speaks a different version (protocol {}); update FreeSize",
            hello.protocol
        )));
    }
    Ok(hello)
}

/// What trusting the Hub's own authority means for a request.
fn trust_for(ca: Option<&str>) -> Trust {
    ca.map_or(Trust::System, |ca| Trust::Authority(ca.to_string()))
}

/// The same address over plain `http://`, where the Hub also answers.
fn plain(hub: &str) -> Option<String> {
    hub.strip_prefix("https://")
        .map(|rest| format!("http://{rest}"))
}

/// The Hub's hello before pairing, when nothing about its certificate is
/// known yet. A Hub with its own authority answers plain http on the same
/// port and names the authority there; the signed pairing answer repeats it,
/// so a wrong one is caught. A Hub with an administrator's certificate or
/// behind a reverse proxy answers over https with a certificate the system
/// already trusts.
fn first_hello(transport: &dyn Transport, hub: &str) -> AgentResult<Hello> {
    let over_https = hello(transport, hub, &Trust::System);
    if over_https.is_ok() {
        return over_https;
    }
    match plain(hub) {
        Some(plain) => hello(transport, &plain, &Trust::System).or(over_https),
        None => over_https,
    }
}

/// Pairs with a Hub using a link from its dashboard, checking the Hub has
/// the key the link names. Without a key in the link, `confirm` is shown the
/// Hub's fingerprint and decides. A Hub serving HTTPS with its own authority
/// is paired over it, whatever the link says, and the device keeps that
/// authority for this Hub's address.
pub fn pair(
    transport: &dyn Transport,
    link: &PairingLink,
    info: DeviceInfo,
    confirm: &mut dyn FnMut(&Hello) -> bool,
) -> AgentResult<AgentState> {
    let hello = first_hello(transport, &link.hub)?;
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
    // With its own authority the Hub speaks HTTPS: pair over it, even from
    // a link written before the Hub did.
    let hub = match (&hello.ca, link.hub.strip_prefix("http://")) {
        (Some(_), Some(rest)) => format!("https://{rest}"),
        _ => link.hub.clone(),
    };
    let trust = trust_for(hello.ca.as_deref());
    let key = KeyPair::generate();
    let name = info.name.clone();
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
            &format!("{hub}/api/v1/pair"),
            &[("Content-Type", "application/json".into())],
            Some(&body),
            TIMEOUT,
            &trust,
        )
        .map_err(|e| AgentError::new(format!("can't reach {hub}: {e}")))?;
    let signed: Signed<PairResponse> = decode(status, &answer)?;
    let response = signed
        .open(&hello.key, Purpose::PairResponse)
        .map_err(|e| AgentError::new(format!("the Hub's answer: {e}")))?;
    if response.device_key != key.public() || response.hub.key != hello.key {
        return Err(AgentError::new(
            "the Hub's answer was meant for another device",
        ));
    }
    if response.hub.ca != hello.ca {
        return Err(AgentError::new(
            "the Hub's signed answer names another certificate authority than it first said. \
             Something between this device and the Hub may be pretending to be it.",
        ));
    }
    let mut state = AgentState::new(hub, hello.name, hello.key, response.device, &key);
    state.ca = hello.ca;
    state.name = name;
    Ok(state)
}

/// Acts on the Hub saying it's reached at another address now (it serves
/// HTTPS since the device paired over plain http). Only a Hub that signs
/// with the key the device paired with, and answers at the new address with
/// that key, is followed. `Ok(true)` when the state changed and should be
/// saved.
pub fn move_to(
    transport: &dyn Transport,
    state: &mut AgentState,
    signed: &Signed<HubAddress>,
) -> AgentResult<bool> {
    let address = signed
        .open(&state.hub_key, Purpose::Move)
        .map_err(|e| AgentError::new(format!("the Hub's new address: {e}")))?;
    let hub = freesize_proto::pairing::normalize_hub(&address.url)
        .ok_or_else(|| AgentError::new(format!("the Hub's new address: {}", address.url)))?;
    if hub == state.hub && address.ca == state.ca {
        return Ok(false);
    }
    let there = hello(transport, &hub, &trust_for(address.ca.as_deref()))?;
    if there.key != state.hub_key {
        return Err(AgentError::new(format!(
            "{hub} answers with another key than the Hub this device paired with"
        )));
    }
    state.hub = hub;
    state.ca = address.ca;
    Ok(true)
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
                &self.state.trust(),
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
        self.poll_request(&PollRequest {
            wait,
            resend: false,
        })
    }

    /// The first poll after starting: everything still open, new or not.
    pub fn poll_all(&self, wait: u32) -> AgentResult<PollResponse> {
        self.poll_request(&PollRequest { wait, resend: true })
    }

    fn poll_request(&self, request: &PollRequest) -> AgentResult<PollResponse> {
        self.post("/api/v1/poll", request, u64::from(request.wait) + TIMEOUT)
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
                freesize_proto::pairing::encode(&fingerprint)
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use freesize_proto::{Hello, HubAddress, KeyPair, PROTOCOL, Purpose, Signed};

    use super::*;

    /// Answers every hello with one Hub's key, and remembers what was asked.
    struct FakeHub {
        key: KeyPair,
        asked: RefCell<Vec<(String, Trust)>>,
    }

    impl Transport for FakeHub {
        fn request(
            &self,
            _method: &str,
            url: &str,
            _headers: &[(&str, String)],
            _body: Option<&[u8]>,
            _timeout_secs: u64,
            trust: &Trust,
        ) -> Result<(u16, Vec<u8>), String> {
            self.asked
                .borrow_mut()
                .push((url.to_string(), trust.clone()));
            let hello = Hello {
                name: "Hub".into(),
                version: "test".into(),
                protocol: PROTOCOL,
                key: self.key.public(),
                ca: None,
            };
            Ok((200, serde_json::to_vec(&hello).unwrap()))
        }
    }

    fn paired(hub_key: &KeyPair) -> AgentState {
        AgentState::new(
            "http://hub.lan:9090".into(),
            "Hub".into(),
            hub_key.public(),
            "d1".into(),
            &KeyPair::generate(),
        )
    }

    #[test]
    fn a_move_needs_the_hubs_signature_and_its_key_at_the_new_address() {
        let hub_key = KeyPair::generate();
        let transport = FakeHub {
            key: KeyPair::from_seed(hub_key.seed()),
            asked: RefCell::new(Vec::new()),
        };
        let address = HubAddress {
            url: "https://hub.lan:9090".into(),
            ca: Some("-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n".into()),
            time: 1,
        };

        // Signed by someone else: refused without a word to anyone.
        let mut state = paired(&hub_key);
        let forged = Signed::sign(&address, &KeyPair::generate(), Purpose::Move);
        assert!(move_to(&transport, &mut state, &forged).is_err());
        assert_eq!(state.hub, "http://hub.lan:9090");
        assert!(transport.asked.borrow().is_empty());

        // Signed by the Hub: followed once the new address answers with the
        // Hub's key, over its authority.
        let signed = Signed::sign(&address, &hub_key, Purpose::Move);
        assert_eq!(move_to(&transport, &mut state, &signed), Ok(true));
        assert_eq!(state.hub, "https://hub.lan:9090");
        assert_eq!(state.ca, address.ca);
        assert_eq!(
            transport.asked.borrow().as_slice(),
            [(
                "https://hub.lan:9090/api/v1/hello".to_string(),
                Trust::Authority(address.ca.clone().unwrap())
            )]
        );
        // The same again changes nothing.
        assert_eq!(move_to(&transport, &mut state, &signed), Ok(false));

        // A Hub with another key at the new address isn't followed.
        let other = FakeHub {
            key: KeyPair::generate(),
            asked: RefCell::new(Vec::new()),
        };
        let mut state = paired(&hub_key);
        assert!(move_to(&other, &mut state, &signed).is_err());
        assert_eq!(state.hub, "http://hub.lan:9090");
    }
}
