//! Nextcloud on the Hub (0.12): an admin connects a Nextcloud account
//! through Login Flow v2 (the Hub starts the flow, the person signs in in
//! their browser, the Hub polls until Nextcloud hands it an app password),
//! and from then on the Hub lists the account's files over WebDAV: each
//! folder's size, each big file's id, size and checksums, and the trash.
//! The listing is what the devices' reports are joined with in `copies`.
//! The Hub reads only; nothing on Nextcloud is changed by anything here.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, State};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use freesize_proto::keys::random_id;
use freesize_proto::sync::BIG_FILE;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Admin;
use crate::db::now;
use crate::error::{ApiError, ApiResult};
use crate::{AppState, audit};

/// How the Hub introduces itself: Nextcloud shows it on the sign-in page
/// and in the account's security settings.
const USER_AGENT: &str = concat!("FreeSize Hub/", env!("CARGO_PKG_VERSION"));
/// How long a person has to sign in once the flow started.
const FLOW_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const FLOW_POLL: Duration = Duration::from_secs(5);
/// Between listings of each account.
const SYNC_EVERY: Duration = Duration::from_secs(6 * 60 * 60);
/// Most folders listed per account per listing; a request each.
const MAX_REQUESTS: usize = 5_000;
/// Folders are listed to this depth whatever their size; deeper only
/// when they hold a big file's worth.
const LIST_DEPTH: usize = 2;
const MAX_DEPTH: usize = 12;
/// Longest WebDAV answer read.
const MAX_BODY: u64 = 64 * 1024 * 1024;

// ----- Accounts -------------------------------------------------------------

/// A connected account, as the dashboard sees it: never the app password.
#[derive(Clone, Debug, Serialize)]
pub struct Account {
    pub id: i64,
    pub server: String,
    /// The account's id once a listing has asked for it, else the name
    /// the person signed in with. Devices report the id.
    pub user: String,
    pub added: i64,
    pub added_by: String,
    /// When the files were last listed, and what went wrong if it failed.
    pub synced: Option<i64>,
    pub error: Option<String>,
    pub files: i64,
    pub folders: i64,
    pub bytes: i64,
    pub trash: i64,
    /// What the server says the account uses, all told, and has left
    /// (-1 when unlimited, or not listed yet).
    pub quota_used: i64,
    pub quota_free: i64,
    /// A listing is running now.
    #[serde(default)]
    pub syncing: bool,
}

pub fn accounts(c: &Connection) -> rusqlite::Result<Vec<Account>> {
    let mut stmt = c.prepare(
        "SELECT id, server, COALESCE(dav_user, user), added, added_by, synced, error, files, folders, bytes, trash,
                quota_used, quota_free
         FROM nextcloud ORDER BY server, user",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Account {
            id: r.get(0)?,
            server: r.get(1)?,
            user: r.get(2)?,
            added: r.get(3)?,
            added_by: r.get(4)?,
            synced: r.get(5)?,
            error: r.get(6)?,
            files: r.get(7)?,
            folders: r.get(8)?,
            bytes: r.get(9)?,
            trash: r.get(10)?,
            quota_used: r.get(11)?,
            quota_free: r.get(12)?,
            syncing: false,
        })
    })?;
    rows.collect()
}

/// What signs the Hub in.
#[derive(Clone)]
pub struct Credentials {
    pub server: String,
    /// The name the person signed in with: the app password's.
    pub user: String,
    pub password: String,
    /// The account's id, once known.
    pub dav_user: Option<String>,
}

pub fn credentials(c: &Connection, id: i64) -> ApiResult<Credentials> {
    c.query_row(
        "SELECT server, user, password, dav_user FROM nextcloud WHERE id = ?1",
        [id],
        |r| {
            Ok(Credentials {
                server: r.get(0)?,
                user: r.get(1)?,
                password: r.get(2)?,
                dav_user: r.get(3)?,
            })
        },
    )
    .optional()?
    .ok_or_else(|| ApiError::not_found("no such Nextcloud account"))
}

/// A server address as typed: `https://` unless said otherwise, no
/// trailing slash or `/index.php`.
pub fn clean_server(server: &str) -> ApiResult<String> {
    let mut server = server.trim().to_string();
    if server.is_empty() {
        return Err(ApiError::bad_request("give the Nextcloud server's address"));
    }
    if !server.starts_with("https://") && !server.starts_with("http://") {
        server = format!("https://{server}");
    }
    let server = server.trim_end_matches('/');
    let server = server.strip_suffix("/index.php").unwrap_or(server);
    if server.contains(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
    {
        return Err(ApiError::bad_request("that's not a server address"));
    }
    Ok(server.trim_end_matches('/').to_string())
}

/// Servers and users compare without the scheme or case, so a device's
/// `https://Cloud.example.com/` and the Hub's `https://cloud.example.com`
/// are the same account.
pub fn account_key(server: &str, user: &str) -> (String, String) {
    let server = server.trim().to_lowercase();
    let server = server
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/');
    (server.to_string(), user.trim().to_lowercase())
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .http_status_as_error(false)
        .user_agent(USER_AGENT)
        // PROPFIND, which WebDAV listings are.
        .allow_non_standard_methods(true)
        .build()
        .into()
}

fn basic(creds: &Credentials) -> String {
    format!(
        "Basic {}",
        STANDARD.encode(format!("{}:{}", creds.user, creds.password))
    )
}

// ----- Login Flow v2 --------------------------------------------------------

/// A sign-in under way: the person is at `login_url`; the Hub polls.
pub struct Flow {
    pub server: String,
    pub login_url: String,
    pub started: Instant,
    pub state: FlowState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum FlowState {
    Waiting,
    Done { account: i64, user: String },
    Failed { error: String },
}

/// The sign-ins under way, by their id.
#[derive(Default)]
pub struct Logins {
    flows: Mutex<HashMap<String, Flow>>,
    /// Accounts whose files are being listed now.
    syncing: Mutex<HashSet<i64>>,
}

impl Logins {
    pub fn get(&self, id: &str) -> Option<(String, String, FlowState)> {
        self.flows
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .map(|f| (f.server.clone(), f.login_url.clone(), f.state.clone()))
    }

    fn set(&self, id: &str, state: FlowState) {
        let mut flows = self.flows.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(flow) = flows.get_mut(id) {
            flow.state = state;
        }
        // Finished flows are kept an hour for the page to read, then dropped.
        flows.retain(|_, f| {
            f.state == FlowState::Waiting || f.started.elapsed() < Duration::from_secs(3600)
        });
    }

    pub fn is_syncing(&self, id: i64) -> bool {
        self.syncing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&id)
    }
}

#[derive(Deserialize)]
struct FlowStart {
    poll: FlowPoll,
    login: String,
}

#[derive(Deserialize)]
struct FlowPoll {
    token: String,
    endpoint: String,
}

#[derive(Deserialize)]
struct FlowResult {
    server: String,
    #[serde(rename = "loginName")]
    login_name: String,
    #[serde(rename = "appPassword")]
    app_password: String,
}

/// Starts the flow: where to send the person, and where to poll.
/// Blocking.
fn start_flow(server: &str) -> Result<FlowStart, String> {
    let url = format!("{server}/index.php/login/v2");
    let mut response = agent()
        .post(&url)
        .header("Accept", "application/json")
        .send_empty()
        .map_err(|e| format!("{server}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "{server} answered {} to the sign-in request; is it a Nextcloud server?",
            response.status()
        ));
    }
    response
        .body_mut()
        .read_json()
        .map_err(|e| format!("{server}: the sign-in answer wasn't understood: {e}"))
}

/// Asks once whether the person has signed in. Blocking.
fn poll_flow(http: &ureq::Agent, poll: &FlowPoll) -> Result<Option<FlowResult>, String> {
    let mut response = http
        .post(&poll.endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/x-www-form-urlencoded")
        .send(format!("token={}", percent_encode(&poll.token)).as_bytes())
        .map_err(|e| e.to_string())?;
    match response.status().as_u16() {
        404 => Ok(None),
        200 => response
            .body_mut()
            .read_json()
            .map(Some)
            .map_err(|e| format!("the sign-in answer wasn't understood: {e}")),
        other => Err(format!("the sign-in poll answered {other}")),
    }
}

/// Stores a finished sign-in, replacing an older connection of the same
/// account.
fn store_account(c: &mut Connection, creds: &Credentials, admin: &str) -> rusqlite::Result<i64> {
    let tx = c.transaction()?;
    let existing: Option<i64> = tx
        .query_row(
            "SELECT id FROM nextcloud WHERE server = ?1 AND user = ?2",
            params![creds.server, creds.user],
            |r| r.get(0),
        )
        .optional()?;
    let id = match existing {
        Some(id) => {
            tx.execute(
                "UPDATE nextcloud SET password = ?1, added = ?2, added_by = ?3, error = NULL
                 WHERE id = ?4",
                params![creds.password, now(), admin, id],
            )?;
            id
        }
        None => {
            tx.execute(
                "INSERT INTO nextcloud (server, user, password, added, added_by)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![creds.server, creds.user, creds.password, now(), admin],
            )?;
            tx.last_insert_rowid()
        }
    };
    audit::append(
        &tx,
        admin,
        "Nextcloud connected",
        None,
        &json!({"server": creds.server, "user": creds.user}),
    )?;
    tx.commit()?;
    Ok(id)
}

/// Polls until the person signed in or the time ran out, then stores the
/// account and lists its files.
async fn finish_flow(state: AppState, id: String, poll: FlowPoll, admin: String) {
    let http = agent();
    let started = Instant::now();
    let result = loop {
        tokio::time::sleep(FLOW_POLL).await;
        if started.elapsed() > FLOW_TIMEOUT {
            break Err("nobody signed in within 20 minutes".to_string());
        }
        let http = http.clone();
        let poll = FlowPoll {
            token: poll.token.clone(),
            endpoint: poll.endpoint.clone(),
        };
        match tokio::task::spawn_blocking(move || poll_flow(&http, &poll)).await {
            Ok(Ok(Some(result))) => break Ok(result),
            Ok(Ok(None)) => {}
            Ok(Err(err)) => break Err(err),
            Err(err) => break Err(err.to_string()),
        }
    };
    match result {
        Ok(result) => {
            let creds = Credentials {
                server: result.server.trim_end_matches('/').to_string(),
                user: result.login_name,
                password: result.app_password,
                dav_user: None,
            };
            let stored_creds = creds.clone();
            let stored_admin = admin.clone();
            let stored = state
                .db
                .call(move |c| store_account(c, &stored_creds, &stored_admin))
                .await;
            match stored {
                Ok(account) => {
                    tracing::info!("Nextcloud: {} on {} connected", creds.user, creds.server);
                    state.nextcloud.set(
                        &id,
                        FlowState::Done {
                            account,
                            user: creds.user,
                        },
                    );
                    tokio::spawn(sync_account(state.clone(), account));
                }
                Err(err) => {
                    tracing::error!("Nextcloud: storing the account: {err}");
                    state.nextcloud.set(
                        &id,
                        FlowState::Failed {
                            error: "database error".into(),
                        },
                    );
                }
            }
        }
        Err(error) => {
            tracing::warn!("Nextcloud sign-in: {error}");
            state.nextcloud.set(&id, FlowState::Failed { error });
        }
    }
}

// ----- WebDAV ---------------------------------------------------------------

/// One file or folder as the server lists it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// `/` for the account's root, else `/Photos/2019` or `/Photos/a.jpg`.
    pub path: String,
    pub fileid: i64,
    /// A folder's size is everything in it.
    pub size: u64,
    pub is_dir: bool,
    pub modified: i64,
    /// "SHA1:…", "MD5:…", "SHA256:…" as the server has them.
    pub checksums: Vec<String>,
}

/// What a listing found.
#[derive(Clone, Debug, Default)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub quota_used: u64,
    /// What's left, or -1 when the server doesn't say (unlimited).
    pub quota_free: i64,
    pub trash: u64,
    pub requests: usize,
    /// The account's id, when the server said it.
    pub dav_user: Option<String>,
}

const PROPFIND: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<d:propfind xmlns:d="DAV:" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns">
  <d:prop>
    <d:resourcetype/>
    <d:getcontentlength/>
    <d:getlastmodified/>
    <d:quota-used-bytes/>
    <d:quota-available-bytes/>
    <oc:fileid/>
    <oc:size/>
    <oc:checksums/>
  </d:prop>
</d:propfind>
"#;

/// One PROPFIND. Blocking.
fn propfind(
    http: &ureq::Agent,
    creds: &Credentials,
    url: &str,
    depth: &str,
) -> Result<String, String> {
    let request = ureq::http::Request::builder()
        .method("PROPFIND")
        .uri(url)
        .header("Authorization", basic(creds))
        .header("Depth", depth)
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(PROPFIND)
        .map_err(|e| e.to_string())?;
    let mut response = http.run(request).map_err(|e| format!("{url}: {e}"))?;
    let status = response.status().as_u16();
    if status == 401 || status == 403 {
        return Err(format!(
            "{} refused the Hub's sign-in ({status}); connect the account again",
            creds.server
        ));
    }
    if status != 207 {
        return Err(format!("{url}: HTTP {status}"));
    }
    response
        .body_mut()
        .with_config()
        .limit(MAX_BODY)
        .read_to_string()
        .map_err(|e| format!("{url}: {e}"))
}

/// Creates a folder in the account (`MKCOL`), and the folders above it
/// as needed: `/Archive/Desktop/GoPro` makes three. A folder that's
/// already there is fine. Blocking.
pub fn mkcol(creds: &Credentials, path: &str) -> Result<(), String> {
    let http = agent();
    let (id, _) = account_id(&http, creds);
    let user = percent_encode(&id);
    let base = format!("{}/remote.php/dav/files/{user}", creds.server);
    let mut so_far = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        so_far.push('/');
        so_far.push_str(part);
        let url = format!("{base}{}", encode_path(&so_far));
        let request = ureq::http::Request::builder()
            .method("MKCOL")
            .uri(&url)
            .header("Authorization", basic(creds))
            .body(())
            .map_err(|e| e.to_string())?;
        let response = http.run(request).map_err(|e| format!("{url}: {e}"))?;
        match response.status().as_u16() {
            // Made, or already there.
            201 | 405 => {}
            401 | 403 => {
                return Err(format!(
                    "{} refused the Hub's sign-in; connect the account again",
                    creds.server
                ));
            }
            status => return Err(format!("{url}: HTTP {status}")),
        }
    }
    Ok(())
}

/// Lists the account: every folder two levels down, deeper where a folder
/// holds a big file's worth, and the trash's total. Blocking, and up to
/// a few thousand requests on a big account.
pub fn list_files(creds: &Credentials) -> Result<Listing, String> {
    let http = agent();
    let (id, lookup_error) = account_id(&http, creds);
    let user = percent_encode(&id);
    let base = format!("{}/remote.php/dav/files/{user}", creds.server);
    let prefix = files_prefix(&creds.server, &id);
    let mut listing = Listing {
        quota_free: -1,
        dav_user: lookup_error.is_none().then(|| id.clone()),
        ..Listing::default()
    };
    let mut unmatched: Option<String> = None;
    let mut seen: HashMap<String, usize> = HashMap::new();
    let mut queue: Vec<(String, usize)> = vec![("/".to_string(), 0)];
    while let Some((folder, depth)) = queue.pop() {
        if listing.requests >= MAX_REQUESTS {
            break;
        }
        listing.requests += 1;
        let url = format!("{base}{}", encode_path(&folder));
        let body = propfind(&http, creds, &url, "1")?;
        for response in parse_multistatus(&body) {
            let Some(path) = dav_path(&response.href, &prefix) else {
                unmatched.get_or_insert_with(|| response.href.clone());
                continue;
            };
            if depth == 0 && path == "/" {
                if let Some(used) = response.quota_used {
                    listing.quota_used = used;
                }
                if let Some(free) = response.quota_free {
                    listing.quota_free = free;
                }
            }
            let entry = Entry {
                path: path.clone(),
                fileid: response.fileid.unwrap_or(0),
                size: response.size.or(response.content_length).unwrap_or(0),
                is_dir: response.is_dir,
                modified: response.modified.unwrap_or(0),
                checksums: response.checksums,
            };
            let is_new = !seen.contains_key(&path);
            match seen.get(&path) {
                Some(&at) => listing.entries[at] = entry.clone(),
                None => {
                    seen.insert(path.clone(), listing.entries.len());
                    listing.entries.push(entry.clone());
                }
            }
            if is_new
                && entry.is_dir
                && path != folder
                && depth + 1 < MAX_DEPTH
                && (depth < LIST_DEPTH || entry.size >= BIG_FILE)
            {
                queue.push((path, depth + 1));
            }
        }
    }
    // The root always comes back with its folder; without it, nothing in
    // the answer was understood, and an empty listing would make every
    // device's copy look like the only one.
    if !seen.contains_key("/") {
        let mut error = format!(
            "{} answered, but the Hub couldn't read the listing of {id}'s files",
            creds.server
        );
        if let Some(href) = unmatched {
            error.push_str(&format!(" (it named {href}, not {prefix}/…)"));
        }
        if let Some(lookup) = lookup_error {
            error.push_str(&format!("; asking for the account's id failed: {lookup}"));
        }
        return Err(error);
    }
    // Deeper folders were pushed last and so listed first; put the
    // listing in path order.
    listing.entries.sort_by(|a, b| a.path.cmp(&b.path));
    let trash_url = format!("{}/remote.php/dav/trashbin/{user}/trash", creds.server);
    let trash_prefix = format!(
        "{}/remote.php/dav/trashbin/{id}/trash",
        base_path(&creds.server)
    );
    match propfind(&http, creds, &trash_url, "1") {
        Ok(body) => {
            listing.requests += 1;
            listing.trash = parse_multistatus(&body)
                .into_iter()
                .filter(|r| dav_path(&r.href, &trash_prefix).is_some_and(|p| p != "/"))
                .map(|r| r.size.or(r.content_length).unwrap_or(0))
                .sum();
        }
        Err(err) => tracing::debug!("Nextcloud trash: {err}"),
    }
    Ok(listing)
}

/// The path part of a server address: `/nextcloud` for
/// `https://nas/nextcloud`, empty for a server at the top.
fn base_path(server: &str) -> &str {
    let rest = server.split_once("://").map_or(server, |(_, r)| r);
    rest.find('/')
        .map_or("", |at| rest[at..].trim_end_matches('/'))
}

/// Where an account's files are in WebDAV hrefs, not percent-encoded.
fn files_prefix(server: &str, user: &str) -> String {
    format!("{}/remote.php/dav/files/{user}", base_path(server))
}

/// The account's id, which names its folder in WebDAV, and why asking for
/// it failed. Login Flow hands over the name the person signed in with,
/// which can be their email address instead; that name stands in when the
/// server won't say, and the reason goes into a failed listing's error.
fn account_id(http: &ureq::Agent, creds: &Credentials) -> (String, Option<String>) {
    if let Some(id) = &creds.dav_user {
        return (id.clone(), None);
    }
    let url = format!("{}/ocs/v2.php/cloud/user?format=json", creds.server);
    let answer = http
        .get(&url)
        .header("Authorization", basic(creds))
        .header("OCS-APIRequest", "true")
        .call()
        .map_err(|e| format!("{url}: {e}"))
        .and_then(|mut r| match r.status().as_u16() {
            200 => r
                .body_mut()
                .read_to_string()
                .map_err(|e| format!("{url}: {e}")),
            status => Err(format!("{url}: HTTP {status}")),
        });
    let error = match answer.map(|a| parse_user_id(&a)) {
        Ok(Some(id)) => return (id, None),
        Ok(None) => format!("{url} didn't name the account"),
        Err(error) => error,
    };
    tracing::warn!("Nextcloud: {error}; using the sign-in name");
    (creds.user.clone(), Some(error))
}

/// `ocs.data.id` of a `cloud/user` answer.
fn parse_user_id(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let id = value.pointer("/ocs/data/id")?.as_str()?.trim();
    (!id.is_empty()).then(|| id.to_string())
}

/// The path under the account's files an href names: `/` for the root.
/// `prefix` is from [`files_prefix`]; an href may also be a whole URL.
fn dav_path(href: &str, prefix: &str) -> Option<String> {
    let decoded = percent_decode(href);
    let path = match decoded.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("", |at| &rest[at..]),
        None => decoded.as_str(),
    };
    let rest = path.strip_prefix(prefix)?;
    let rest = rest.trim_end_matches('/');
    Some(if rest.is_empty() {
        "/".to_string()
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        return None;
    })
}

/// Percent-encodes everything but unreserved characters.
pub fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// A path with `/` between names, each name encoded.
fn encode_path(path: &str) -> String {
    if path == "/" {
        return "/".to_string();
    }
    path.split('/')
        .map(percent_encode)
        .collect::<Vec<_>>()
        .join("/")
}

pub fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3])
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ----- The multistatus answer -----------------------------------------------

/// One `d:response` of a PROPFIND answer, the parts the Hub reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DavResponse {
    pub href: String,
    pub fileid: Option<i64>,
    pub size: Option<u64>,
    pub content_length: Option<u64>,
    pub is_dir: bool,
    pub modified: Option<i64>,
    pub checksums: Vec<String>,
    pub quota_used: Option<u64>,
    /// Negative on an unlimited account (-3 for "unlimited" in Nextcloud).
    pub quota_free: Option<i64>,
}

/// Reads the responses out of the XML, whatever prefixes the server uses:
/// elements are told apart by their local names.
pub fn parse_multistatus(xml: &str) -> Vec<DavResponse> {
    let mut responses = Vec::new();
    let mut current: Option<DavResponse> = None;
    let mut stack: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut rest = xml;
    while let Some(start) = rest.find('<') {
        text.push_str(&rest[..start]);
        rest = &rest[start..];
        if rest.starts_with("<?") {
            let Some(end) = rest.find("?>") else { break };
            rest = &rest[end + 2..];
            continue;
        }
        if rest.starts_with("<!--") {
            let Some(end) = rest.find("-->") else { break };
            rest = &rest[end + 3..];
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            let Some(end) = rest.find("]]>") else { break };
            text.push_str(&rest[9..end]);
            rest = &rest[end + 3..];
            continue;
        }
        let Some(end) = rest.find('>') else { break };
        let tag = &rest[1..end];
        rest = &rest[end + 1..];
        if let Some(name) = tag.strip_prefix('/') {
            let name = local_name(name.trim());
            let value = decode_entities(text.trim());
            text.clear();
            if let Some(response) = current.as_mut() {
                close(response, &name, &value, &stack);
            }
            if name == "response"
                && let Some(done) = current.take()
            {
                responses.push(done);
            }
            stack.pop();
        } else {
            let self_closing = tag.ends_with('/');
            let name = local_name(
                tag.trim_end_matches('/')
                    .split(|c: char| c.is_whitespace())
                    .next()
                    .unwrap_or_default(),
            );
            text.clear();
            if name == "response" {
                current = Some(DavResponse::default());
            }
            if self_closing {
                if let Some(response) = current.as_mut() {
                    close(response, &name, "", &stack);
                }
            } else {
                stack.push(name);
            }
        }
    }
    responses
}

fn local_name(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_string()
}

fn close(response: &mut DavResponse, name: &str, value: &str, stack: &[String]) {
    let parent = |n: usize| stack.iter().rev().nth(n).map(String::as_str);
    match name {
        // The element itself is still on the stack when it closes.
        "href" if parent(1) == Some("response") => response.href = value.to_string(),
        "fileid" => response.fileid = value.parse().ok(),
        "size" => response.size = value.parse().ok(),
        "getcontentlength" => response.content_length = value.parse().ok(),
        "collection" => response.is_dir = true,
        "getlastmodified" => response.modified = parse_http_date(value),
        "checksum" => {
            if !value.is_empty() {
                response
                    .checksums
                    .extend(value.split_whitespace().map(str::to_string));
            }
        }
        "quota-used-bytes" => response.quota_used = value.parse().ok(),
        "quota-available-bytes" => response.quota_free = value.parse().ok(),
        _ => {}
    }
}

fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ => {
                let code = entity
                    .strip_prefix("#x")
                    .and_then(|h| u32::from_str_radix(h, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()));
                match code.and_then(char::from_u32) {
                    Some(c) => out.push(c),
                    None => out.push_str(&rest[..=end]),
                }
            }
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// "Mon, 12 Oct 2026 10:00:00 GMT" as seconds since 1970.
pub fn parse_http_date(text: &str) -> Option<i64> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    // Day-name, day, month, year, time, zone.
    if parts.len() < 5 {
        return None;
    }
    let day: i64 = parts[1].parse().ok()?;
    let month = match parts[2] {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year: i64 = parts[3].parse().ok()?;
    let mut time = parts[4].split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second: i64 = time.next()?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a date in the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

// ----- Listing accounts -----------------------------------------------------

/// Stores a listing: the account's files replaced in one transaction.
fn store_listing(c: &mut Connection, id: i64, listing: &Listing) -> rusqlite::Result<()> {
    let tx = c.transaction()?;
    tx.execute("DELETE FROM nextcloud_files WHERE account_id = ?1", [id])?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO nextcloud_files (account_id, path, fileid, size, is_dir, modified, checksums)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for entry in &listing.entries {
            insert.execute(params![
                id,
                entry.path,
                entry.fileid,
                entry.size as i64,
                entry.is_dir,
                entry.modified,
                entry.checksums.join(" ")
            ])?;
        }
    }
    let root = listing.entries.iter().find(|e| e.path == "/");
    let files = listing.entries.iter().filter(|e| !e.is_dir).count() as i64;
    let folders = listing
        .entries
        .iter()
        .filter(|e| e.is_dir && e.path != "/")
        .count() as i64;
    tx.execute(
        "UPDATE nextcloud SET synced = ?1, error = NULL, files = ?2, folders = ?3, bytes = ?4,
                trash = ?5, quota_used = ?6, quota_free = ?7,
                dav_user = COALESCE(?9, dav_user)
         WHERE id = ?8",
        params![
            now(),
            files,
            folders,
            root.map_or(0, |r| r.size as i64),
            listing.trash as i64,
            listing.quota_used as i64,
            listing.quota_free.max(-1),
            id,
            listing.dav_user
        ],
    )?;
    tx.commit()
}

/// Lists one account's files and stores them, unless a listing is
/// already running for it.
pub async fn sync_account(state: AppState, id: i64) {
    {
        let mut syncing = state
            .nextcloud
            .syncing
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if !syncing.insert(id) {
            return;
        }
    }
    let creds = state.db.call(move |c| credentials(c, id)).await;
    let outcome = match creds {
        Ok(creds) => {
            let (server, user) = (creds.server.clone(), creds.user.clone());
            match tokio::task::spawn_blocking(move || list_files(&creds)).await {
                Ok(Ok(listing)) => {
                    tracing::info!(
                        "Nextcloud: listed {} on {}: {} entries in {} requests",
                        user,
                        server,
                        listing.entries.len(),
                        listing.requests
                    );
                    state
                        .db
                        .call(move |c| store_listing(c, id, &listing))
                        .await
                        .map_err(|e| format!("database: {e}"))
                }
                Ok(Err(err)) => Err(err),
                Err(err) => Err(err.to_string()),
            }
        }
        Err(err) => Err(err.message),
    };
    if let Err(error) = outcome {
        tracing::warn!("Nextcloud: listing account {id}: {error}");
        let _ = state
            .db
            .call(move |c| {
                c.execute(
                    "UPDATE nextcloud SET error = ?1 WHERE id = ?2",
                    params![error, id],
                )
            })
            .await;
    }
    state
        .nextcloud
        .syncing
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id);
}

/// Lists every account at start and then every few hours.
pub async fn run(state: AppState) {
    let mut every = tokio::time::interval(SYNC_EVERY);
    every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        every.tick().await;
        let ids: Vec<i64> = state
            .db
            .call(|c| accounts(c).map(|a| a.into_iter().map(|a| a.id).collect()))
            .await
            .unwrap_or_default();
        for id in ids {
            sync_account(state.clone(), id).await;
        }
    }
}

// ----- The API --------------------------------------------------------------

/// `GET /api/nextcloud`: the connected accounts.
pub async fn list(
    State(state): State<AppState>,
    _user: crate::auth::User,
) -> ApiResult<Json<Vec<Account>>> {
    let mut list = state.db.call(|c| accounts(c)).await?;
    for account in &mut list {
        account.syncing = state.nextcloud.is_syncing(account.id);
    }
    Ok(Json(list))
}

#[derive(Deserialize)]
pub struct ConnectRequest {
    server: String,
}

/// `POST /api/nextcloud/connect`: starts a sign-in; the answer carries the
/// page to open and the flow to ask about.
pub async fn connect(
    State(state): State<AppState>,
    Admin(user): Admin,
    Json(request): Json<ConnectRequest>,
) -> ApiResult<Json<Value>> {
    let server = clean_server(&request.server)?;
    let flow_server = server.clone();
    let start = tokio::task::spawn_blocking(move || start_flow(&flow_server))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
        .map_err(ApiError::bad_request)?;
    let id = random_id::<16>();
    state
        .nextcloud
        .flows
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            id.clone(),
            Flow {
                server: server.clone(),
                login_url: start.login.clone(),
                started: Instant::now(),
                state: FlowState::Waiting,
            },
        );
    tokio::spawn(finish_flow(
        state.clone(),
        id.clone(),
        start.poll,
        user.name.clone(),
    ));
    Ok(Json(
        json!({"flow": id, "login_url": start.login, "server": server}),
    ))
}

/// `GET /api/nextcloud/connect/{flow}`: whether the person has signed in.
pub async fn connect_status(
    State(state): State<AppState>,
    Admin(_): Admin,
    Path(flow): Path<String>,
) -> ApiResult<Json<Value>> {
    let (server, login_url, flow_state) = state
        .nextcloud
        .get(&flow)
        .ok_or_else(|| ApiError::not_found("no such sign-in"))?;
    let mut value = serde_json::to_value(flow_state).unwrap_or_default();
    value["server"] = json!(server);
    value["login_url"] = json!(login_url);
    Ok(Json(value))
}

/// Revokes the app password on the server, so it stops working even if a
/// copy of the database is around. Blocking; failing is fine.
fn revoke(creds: &Credentials) -> Result<(), String> {
    let request = ureq::http::Request::builder()
        .method("DELETE")
        .uri(format!("{}/ocs/v2.php/core/apppassword", creds.server))
        .header("Authorization", basic(creds))
        .header("OCS-APIRequest", "true")
        .body(())
        .map_err(|e| e.to_string())?;
    let response = agent().run(request).map_err(|e| e.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("HTTP {}", response.status()))
    }
}

/// `DELETE /api/nextcloud/{id}`: forgets the account and its listing, and
/// asks the server to drop the app password.
pub async fn remove(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    let creds = state.db.call(move |c| credentials(c, id)).await?;
    let revoked = creds.clone();
    let revoked = tokio::task::spawn_blocking(move || revoke(&revoked))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
    if let Err(err) = &revoked {
        tracing::warn!(
            "Nextcloud: couldn't revoke the app password on {}: {err}; remove it in Nextcloud's \
             security settings",
            creds.server
        );
    }
    let name = user.name.clone();
    let server = creds.server.clone();
    let account_user = creds.user.clone();
    let revoked_ok = revoked.is_ok();
    state
        .db
        .call(move |c| -> rusqlite::Result<()> {
            let tx = c.transaction()?;
            tx.execute("DELETE FROM nextcloud_files WHERE account_id = ?1", [id])?;
            tx.execute("DELETE FROM nextcloud WHERE id = ?1", [id])?;
            audit::append(
                &tx,
                &name,
                "Nextcloud disconnected",
                None,
                &json!({"server": server, "user": account_user, "password_revoked": revoked_ok}),
            )?;
            tx.commit()
        })
        .await?;
    Ok(Json(json!({"ok": true, "revoked": revoked_ok})))
}

/// `POST /api/nextcloud/{id}/sync`: lists the files now.
pub async fn sync_now(
    State(state): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<i64>,
) -> ApiResult<Json<Value>> {
    state.db.call(move |c| credentials(c, id)).await?;
    tokio::spawn(sync_account(state.clone(), id));
    Ok(Json(json!({"ok": true})))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ANSWER: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns">
 <d:response>
  <d:href>/remote.php/dav/files/bob/</d:href>
  <d:propstat>
   <d:prop>
    <d:resourcetype><d:collection/></d:resourcetype>
    <d:getlastmodified>Mon, 12 Oct 2026 10:00:00 GMT</d:getlastmodified>
    <d:quota-used-bytes>5000</d:quota-used-bytes>
    <oc:fileid>10</oc:fileid>
    <oc:size>5000</oc:size>
   </d:prop>
   <d:status>HTTP/1.1 200 OK</d:status>
  </d:propstat>
  <d:propstat>
   <d:prop><d:getcontentlength/><oc:checksums/></d:prop>
   <d:status>HTTP/1.1 404 Not Found</d:status>
  </d:propstat>
 </d:response>
 <d:response>
  <d:href>/remote.php/dav/files/bob/My%20Photos/</d:href>
  <d:propstat>
   <d:prop>
    <d:resourcetype><d:collection/></d:resourcetype>
    <oc:fileid>11</oc:fileid>
    <oc:size>4000</oc:size>
   </d:prop>
   <d:status>HTTP/1.1 200 OK</d:status>
  </d:propstat>
 </d:response>
 <d:response>
  <d:href>/remote.php/dav/files/bob/a%26b.txt</d:href>
  <d:propstat>
   <d:prop>
    <d:resourcetype/>
    <d:getcontentlength>1000</d:getcontentlength>
    <d:getlastmodified>Thu, 01 Jan 1970 00:00:10 GMT</d:getlastmodified>
    <oc:fileid>12</oc:fileid>
    <oc:size>1000</oc:size>
    <oc:checksums><oc:checksum>SHA1:abc MD5:def</oc:checksum></oc:checksums>
   </d:prop>
   <d:status>HTTP/1.1 200 OK</d:status>
  </d:propstat>
 </d:response>
</d:multistatus>"#;

    #[test]
    fn reads_a_multistatus_answer() {
        let responses = parse_multistatus(ANSWER);
        assert_eq!(responses.len(), 3);
        let root = &responses[0];
        assert_eq!(root.href, "/remote.php/dav/files/bob/");
        assert!(root.is_dir);
        assert_eq!(root.fileid, Some(10));
        assert_eq!(root.size, Some(5000));
        assert_eq!(root.quota_used, Some(5000));
        assert_eq!(root.modified, Some(1_791_799_200));
        assert_eq!(root.content_length, None);
        let photos = &responses[1];
        assert_eq!(
            dav_path(&photos.href, "/remote.php/dav/files/bob"),
            Some("/My Photos".into())
        );
        let file = &responses[2];
        assert!(!file.is_dir);
        assert_eq!(file.content_length, Some(1000));
        assert_eq!(file.modified, Some(10));
        assert_eq!(file.checksums, ["SHA1:abc", "MD5:def"]);
        assert_eq!(
            dav_path(&file.href, "/remote.php/dav/files/bob"),
            Some("/a&b.txt".into())
        );
        assert_eq!(
            dav_path(&root.href, "/remote.php/dav/files/bob"),
            Some("/".into())
        );
        assert_eq!(dav_path("/other/", "/remote.php/dav/files/bob"), None);
    }

    #[test]
    fn an_account_signed_in_by_email_matches_devices_by_its_id() {
        // Devices report the account's id (the client's dav_user), so
        // once a listing has learned it, the account goes by it.
        let db = crate::db::Db::memory().unwrap();
        db.now(|c| {
            let creds = Credentials {
                server: "https://cloud.example.com".into(),
                user: "bob@example.com".into(),
                password: "app".into(),
                dav_user: None,
            };
            let id = store_account(c, &creds, "admin").unwrap();
            assert_eq!(accounts(c).unwrap()[0].user, "bob@example.com");
            let listing = Listing {
                entries: vec![Entry {
                    path: "/".into(),
                    is_dir: true,
                    ..Entry::default()
                }],
                quota_free: -1,
                dav_user: Some("rob".into()),
                ..Listing::default()
            };
            store_listing(c, id, &listing).unwrap();
            assert_eq!(accounts(c).unwrap()[0].user, "rob");
            let stored = credentials(c, id).unwrap();
            assert_eq!(stored.user, "bob@example.com");
            assert_eq!(stored.dav_user.as_deref(), Some("rob"));
            // A later listing that couldn't ask keeps what was learned.
            let listing = Listing {
                dav_user: None,
                ..listing
            };
            store_listing(c, id, &listing).unwrap();
            assert_eq!(accounts(c).unwrap()[0].user, "rob");
        });
    }

    #[test]
    fn listing_paths_for_a_user_signed_in_by_email() {
        // Signed in with an email address: the hrefs carry it
        // percent-encoded, or the account's own id instead, and a server
        // under a folder puts its path in front.
        let prefix = files_prefix("https://cloud.example.com", "bob@example.com");
        assert_eq!(
            dav_path("/remote.php/dav/files/bob%40example.com/Photos/", &prefix),
            Some("/Photos".into())
        );
        let under = files_prefix("http://nas:8080/nextcloud/", "bob");
        assert_eq!(under, "/nextcloud/remote.php/dav/files/bob");
        assert_eq!(
            dav_path("/nextcloud/remote.php/dav/files/bob/a.jpg", &under),
            Some("/a.jpg".into())
        );
        assert_eq!(
            dav_path(
                "http://nas:8080/nextcloud/remote.php/dav/files/bob/",
                &under
            ),
            Some("/".into())
        );
        // Only right after the server's own folder, and only this account.
        assert_eq!(dav_path("/remote.php/dav/files/bob/a.jpg", &under), None);
        assert_eq!(
            dav_path("/other/nextcloud/remote.php/dav/files/bob/a.jpg", &under),
            None
        );
        assert_eq!(
            dav_path("/nextcloud/remote.php/dav/files/bobby/a.jpg", &under),
            None
        );
        assert_eq!(
            parse_user_id(
                r#"{"ocs":{"meta":{"status":"ok"},"data":{"id":"rob","email":"bob@example.com"}}}"#
            ),
            Some("rob".into())
        );
        assert_eq!(parse_user_id(r#"{"ocs":{"data":[]}}"#), None);
        assert_eq!(parse_user_id("<html>"), None);
    }

    #[test]
    fn dates_paths_and_entities() {
        assert_eq!(parse_http_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(
            parse_http_date("Tue, 19 Jan 2038 03:14:07 GMT"),
            Some(2_147_483_647)
        );
        assert_eq!(parse_http_date("soon"), None);
        assert_eq!(encode_path("/My Photos/a&b.txt"), "/My%20Photos/a%26b.txt");
        assert_eq!(percent_decode("a%20b%zz%2"), "a b%zz%2");
        assert_eq!(
            decode_entities("a &amp; b &lt;c&gt; &#65;&#x42; &bogus;"),
            "a & b <c> AB &bogus;"
        );
        assert_eq!(
            account_key("https://Cloud.Example.com/", "Bob"),
            ("cloud.example.com".into(), "bob".into())
        );
        assert_eq!(
            clean_server("cloud.example.com/index.php/").unwrap(),
            "https://cloud.example.com"
        );
        assert_eq!(
            clean_server("http://nas:8080/nextcloud/").unwrap(),
            "http://nas:8080/nextcloud"
        );
        assert!(clean_server("").is_err());
        assert!(clean_server("https://a b").is_err());
    }
}
