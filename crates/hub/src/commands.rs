//! Commands for devices: what an admin asked for, signed by the Hub, passed
//! to the device, and what the device made of it. The Hub only asks: each
//! device checks the signature, its own rules, and gets a person's
//! approval (its own, or its designated approver's) before doing anything.

use axum::Json;
use axum::extract::{Path, Query, State};
use disktree_core::snapshot::FolderSize;
use disktree_proto::keys::random_id;
use disktree_proto::{
    Action, Admin as AdminRef, ApprovalRequest, AppsReport, Command, CommandState, CommandUpdate,
    Decision, PollResponse, PublicKey, Purpose, Signed,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{Admin, User};
use crate::db::now;
use crate::devices::{Device, DeviceRequest, gunzip_json};
use crate::error::{ApiError, ApiResult};
use crate::{AppState, audit};

/// How long a device has to act on a request.
const LIFETIME: i64 = 24 * 60 * 60;
/// A device that hasn't moved a delivered request on in this long gets it
/// again (it may have restarted).
const REDELIVER: i64 = 10 * 60;
/// Most items in one delete request.
const MAX_PATHS: usize = 500;

/// States in which a command is still open.
const OPEN: &str = "('queued', 'delivered', 'shown', 'needs_approver', 'running')";

fn state_name(state: CommandState) -> &'static str {
    match state {
        CommandState::Shown => "shown",
        CommandState::NeedsApprover => "needs_approver",
        CommandState::Denied => "denied",
        CommandState::Expired => "expired",
        CommandState::Running => "running",
        CommandState::Done => "done",
        CommandState::Failed => "failed",
    }
}

/// Marks open commands past their time as expired.
fn expire(c: &Connection) -> rusqlite::Result<()> {
    let mut stmt = c.prepare(&format!(
        "SELECT id, device_id, summary FROM commands WHERE state IN {OPEN} AND expires < ?1"
    ))?;
    let old: Vec<(String, String, String)> = stmt
        .query_map([now()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    for (id, device, summary) in old {
        c.execute(
            "UPDATE commands SET state = 'expired', updated = ?1 WHERE id = ?2",
            params![now(), id],
        )?;
        audit::append(
            c,
            "Hub",
            "request expired",
            Some(&device),
            &json!({"request": id, "summary": summary}),
        )?;
    }
    Ok(())
}

#[derive(Deserialize)]
pub struct NewCommand {
    action: Action,
    #[serde(default)]
    reason: String,
}

/// The device row a command is made for.
struct Target {
    name: String,
    group_id: i64,
    info: disktree_proto::DeviceInfo,
}

/// Size of each path in the device's newest scan of `root`, where known.
fn sizes_in_scan(
    c: &Connection,
    device: &str,
    root: &str,
    paths: &[String],
) -> rusqlite::Result<Option<u64>> {
    let scan: Option<(i64, Vec<u8>)> = c
        .query_row(
            "SELECT id, folders FROM scans WHERE device_id = ?1 AND root = ?2 ORDER BY taken DESC LIMIT 1",
            params![device, root],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((scan, blob)) = scan else {
        return Ok(None);
    };
    let folders: Vec<FolderSize> = gunzip_json(&blob).unwrap_or_default();
    let mut total = 0u64;
    for path in paths {
        if let Some(folder) = folders.iter().find(|f| &f.path == path) {
            total += folder.allocated;
            continue;
        }
        let file: Option<i64> = c
            .query_row(
                "SELECT allocated FROM scan_files WHERE scan_id = ?1 AND path = ?2",
                params![scan, path],
                |r| r.get(0),
            )
            .optional()?;
        total += file.unwrap_or(0) as u64;
    }
    Ok(Some(total))
}

/// Checks a requested action against what the device reported.
fn check_action(
    c: &Connection,
    device: &str,
    target: &Target,
    action: &mut Action,
) -> ApiResult<()> {
    let kind = action.kind();
    if !target.info.actions.contains(&kind) {
        return Err(ApiError::bad_request(format!(
            "{} doesn't {}",
            target.name,
            kind.label()
        )));
    }
    if !target.info.attended && target.info.approver.is_none() {
        return Err(ApiError::conflict(format!(
            "nobody is at {} and it has no approver: set one on the device with `disktree \
             approver set <fingerprint>`",
            target.name
        )));
    }
    match action {
        Action::Delete { root, paths, bytes } => {
            paths.retain(|p| !p.trim().is_empty());
            paths.sort();
            paths.dedup();
            if paths.is_empty() || paths.len() > MAX_PATHS {
                return Err(ApiError::bad_request(format!(
                    "choose 1 to {MAX_PATHS} items"
                )));
            }
            if paths.iter().any(|p| {
                p.starts_with('/') || p.split('/').any(|part| part == ".." || part.is_empty())
            }) {
                return Err(ApiError::bad_request(
                    "paths must be below the scanned folder",
                ));
            }
            *bytes = sizes_in_scan(c, device, root, paths)?.ok_or_else(|| {
                ApiError::bad_request("the device never reported scanning that folder")
            })?;
        }
        Action::Uninstall { package, label } => {
            let report = apps_report(c, device)?;
            let app = report
                .apps
                .iter()
                .find(|a| &a.package == package)
                .ok_or_else(|| ApiError::bad_request("the device didn't report that app"))?;
            *label = app.label.clone();
        }
        Action::CleanJunk { kind, label, bytes } => {
            let report = apps_report(c, device)?;
            let junk = report
                .junk
                .iter()
                .find(|j| &j.kind == kind)
                .ok_or_else(|| ApiError::bad_request("the device didn't report that"))?;
            *label = junk.label.clone();
            *bytes = junk.bytes;
        }
    }
    Ok(())
}

fn apps_report(c: &Connection, device: &str) -> ApiResult<AppsReport> {
    let text: Option<String> = c
        .query_row(
            "SELECT report FROM apps WHERE device_id = ?1",
            [device],
            |r| r.get(0),
        )
        .optional()?;
    text.and_then(|t| serde_json::from_str(&t).ok())
        .ok_or_else(|| ApiError::bad_request("the device hasn't reported its apps"))
}

/// `POST /api/devices/{id}/commands`: asks a device to do something.
pub async fn create(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(device): Path<String>,
    Json(request): Json<NewCommand>,
) -> ApiResult<Json<Value>> {
    let key = state.key.seed();
    let reason: String = request.reason.trim().chars().take(500).collect();
    let config = state.config.clone();
    let device_id = device.clone();
    let id = state
        .db
        .call(move |c| -> ApiResult<String> {
            let target: Target = c
                .query_row(
                    "SELECT name, group_id, info FROM devices WHERE id = ?1 AND removed = 0",
                    [&device],
                    |r| {
                        Ok(Target {
                            name: r.get(0)?,
                            group_id: r.get(1)?,
                            info: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                        })
                    },
                )
                .optional()?
                .ok_or_else(|| ApiError::not_found("no such device"))?;
            let groups = crate::dashboard::groups(c)?;
            let group = groups
                .iter()
                .find(|g| g.id == target.group_id)
                .ok_or_else(|| ApiError::not_found("no such device"))?;
            if !user.sees(&group.viewer_groups, &config) {
                return Err(ApiError::not_found("no such device"));
            }
            let mut action = request.action;
            check_action(c, &device, &target, &mut action)?;
            let command = Command {
                id: random_id::<10>(),
                device: device.clone(),
                action,
                by: AdminRef {
                    subject: user.subject.clone(),
                    name: user.name.clone(),
                },
                reason,
                created: now(),
                expires: now() + LIFETIME,
                grant_minutes: group.grant_minutes.clone(),
            };
            let signed = Signed::sign(
                &command,
                &disktree_proto::KeyPair::from_seed(key),
                Purpose::Command,
            );
            let summary = command.action.summary();
            c.execute(
                "INSERT INTO commands (id, device_id, kind, summary, admin, admin_name, created,
                                       expires, signed, state, updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'queued', ?7)",
                params![
                    command.id,
                    device,
                    serde_json::to_value(command.action.kind())
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default(),
                    summary,
                    user.subject,
                    user.name,
                    command.created,
                    command.expires,
                    serde_json::to_string(&signed).unwrap_or_default(),
                ],
            )?;
            audit::append(
                c,
                &user.name,
                "request made",
                Some(&device),
                &json!({"request": command.id, "summary": summary,
                        "action": command.action, "reason": command.reason}),
            )?;
            Ok(command.id)
        })
        .await?;
    state.wakers.wake(&device_id);
    Ok(Json(json!({"id": id})))
}

#[derive(Serialize)]
pub struct CommandRow {
    pub id: String,
    pub device_id: String,
    pub device_name: String,
    pub kind: String,
    pub summary: String,
    pub admin_name: String,
    pub created: i64,
    pub expires: i64,
    pub state: String,
    pub detail: String,
    pub approval: Option<Value>,
    pub freed: i64,
    pub failed: Vec<String>,
    pub updated: i64,
    pub action: Option<Value>,
}

#[derive(Deserialize)]
pub struct ListQuery {
    device: Option<String>,
}

/// `GET /api/commands`: requests, open ones first, then the newest.
pub async fn list(
    State(state): State<AppState>,
    user: User,
    Query(query): Query<ListQuery>,
) -> ApiResult<Json<Vec<CommandRow>>> {
    let config = state.config.clone();
    let rows = state
        .db
        .call(move |c| -> rusqlite::Result<Vec<CommandRow>> {
            expire(c)?;
            let visible: Vec<i64> = crate::dashboard::groups(c)?
                .into_iter()
                .filter(|g| user.sees(&g.viewer_groups, &config))
                .map(|g| g.id)
                .collect();
            let mut stmt = c.prepare(&format!(
                "SELECT m.id, m.device_id, d.name, m.kind, m.summary, m.admin_name, m.created,
                        m.expires, m.state, m.detail, m.approval, m.freed, m.failed, m.updated,
                        d.group_id, m.signed
                 FROM commands m JOIN devices d ON d.id = m.device_id
                 WHERE (?1 IS NULL OR m.device_id = ?1)
                 ORDER BY m.state IN {OPEN} DESC, m.created DESC LIMIT 300"
            ))?;
            let rows = stmt.query_map([query.device], |r| {
                let signed: String = r.get(15)?;
                let action = serde_json::from_str::<Signed<Command>>(&signed)
                    .ok()
                    .and_then(|s| s.peek().ok())
                    .and_then(|cmd| serde_json::to_value(cmd.action).ok());
                Ok((
                    r.get::<_, i64>(14)?,
                    CommandRow {
                        id: r.get(0)?,
                        device_id: r.get(1)?,
                        device_name: r.get(2)?,
                        kind: r.get(3)?,
                        summary: r.get(4)?,
                        admin_name: r.get(5)?,
                        created: r.get(6)?,
                        expires: r.get(7)?,
                        state: r.get(8)?,
                        detail: r.get(9)?,
                        approval: r
                            .get::<_, Option<String>>(10)?
                            .and_then(|a| serde_json::from_str(&a).ok()),
                        freed: r.get(11)?,
                        failed: serde_json::from_str(&r.get::<_, String>(12)?).unwrap_or_default(),
                        updated: r.get(13)?,
                        action,
                    },
                ))
            })?;
            Ok(rows
                .filter_map(Result::ok)
                .filter(|(group, _)| visible.contains(group))
                .map(|(_, row)| row)
                .collect())
        })
        .await?;
    Ok(Json(rows))
}

/// `POST /api/commands/{id}/cancel`: withdraws a request still waiting.
pub async fn cancel(
    State(state): State<AppState>,
    Admin(user): Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let device = state
        .db
        .call(move |c| -> ApiResult<String> {
            let row: Option<(String, String, String)> = c
                .query_row(
                    "SELECT device_id, state, summary FROM commands WHERE id = ?1",
                    [&id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let (device, current, summary) =
                row.ok_or_else(|| ApiError::not_found("no such request"))?;
            if !["queued", "delivered", "shown", "needs_approver"].contains(&current.as_str()) {
                return Err(ApiError::conflict("that request isn't waiting any more"));
            }
            c.execute(
                "UPDATE commands SET state = 'cancelled', updated = ?1 WHERE id = ?2",
                params![now(), id],
            )?;
            audit::append(
                c,
                &user.name,
                "request withdrawn",
                Some(&device),
                &json!({"request": id, "summary": summary}),
            )?;
            Ok(device)
        })
        .await?;
    state.wakers.wake(&device);
    Ok(Json(json!({"ok": true})))
}

/// Everything waiting for `device`, marked as delivered.
///
/// With `resend`, everything still open goes out again, not only what's new
/// or overdue: the device restarted and lost what it was shown.
pub async fn pending_for(
    state: &AppState,
    device: &Device,
    resend: bool,
) -> ApiResult<PollResponse> {
    let device = device.clone();
    let pending = state
        .db
        .call(move |c| -> rusqlite::Result<PollResponse> {
            expire(c)?;
            let now = now();
            // Delivered before this goes out again.
            let again = if resend { now + 1 } else { now - REDELIVER };
            let mut response = PollResponse::default();

            // Its own commands.
            let mut stmt = c.prepare(
                "SELECT id, signed, state FROM commands WHERE device_id = ?1 AND (state = 'queued'
                   OR (state IN ('delivered', 'shown', 'needs_approver') AND updated < ?2))",
            )?;
            let due: Vec<(String, String, String)> = stmt
                .query_map(params![device.id, again], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, signed, current) in due {
                if let Ok(signed) = serde_json::from_str(&signed) {
                    response.commands.push(signed);
                }
                let next = if current == "queued" { "delivered" } else { current.as_str() };
                c.execute(
                    "UPDATE commands SET state = ?1, delivered = ?2, updated = ?2 WHERE id = ?3",
                    params![next, now, id],
                )?;
            }

            // Requests for devices it approves for.
            let mut stmt = c.prepare(&format!(
                "SELECT a.command_id, m.signed, m.device_id, d.name FROM approvals a
                 JOIN commands m ON m.id = a.command_id JOIN devices d ON d.id = m.device_id
                 WHERE a.approver = ?1 AND a.decision IS NULL AND m.state IN {OPEN}
                   AND (a.delivered IS NULL OR a.delivered < ?2)"
            ))?;
            let asks: Vec<(String, String, String, String)> = stmt
                .query_map(params![device.fingerprint, again], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                })?
                .collect::<rusqlite::Result<_>>()?;
            for (id, signed, target, name) in asks {
                if let Ok(command) = serde_json::from_str(&signed) {
                    response.approvals.push(ApprovalRequest {
                        command,
                        device: target,
                        device_name: name,
                    });
                }
                c.execute("UPDATE approvals SET delivered = ?1 WHERE command_id = ?2", params![now, id])?;
            }

            // Its approver's decisions.
            let mut stmt = c.prepare(&format!(
                "SELECT a.command_id, a.decision FROM approvals a JOIN commands m ON m.id = a.command_id
                 WHERE m.device_id = ?1 AND a.decision IS NOT NULL AND m.state IN {OPEN}
                   AND (a.decision_delivered IS NULL OR a.decision_delivered < ?2)"
            ))?;
            let decided: Vec<(String, String)> = stmt
                .query_map(params![device.id, again], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?;
            for (id, decision) in decided {
                if let Ok(decision) = serde_json::from_str(&decision) {
                    response.decisions.push(decision);
                }
                c.execute(
                    "UPDATE approvals SET decision_delivered = ?1 WHERE command_id = ?2",
                    params![now, id],
                )?;
            }

            // Withdrawn requests it had been given.
            let mut stmt = c.prepare(
                "SELECT id FROM commands WHERE device_id = ?1 AND state = 'cancelled'
                   AND delivered IS NOT NULL AND cancel_delivered IS NULL",
            )?;
            let cancelled: Vec<String> = stmt
                .query_map([&device.id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            for id in cancelled {
                c.execute("UPDATE commands SET cancel_delivered = ?1 WHERE id = ?2", params![now, id])?;
                response.cancelled.push(id);
            }
            Ok(response)
        })
        .await?;
    Ok(pending)
}

/// `POST /api/v1/commands/{id}`: a device says how a command went.
pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<String>,
    request: DeviceRequest<CommandUpdate>,
) -> ApiResult<Json<Value>> {
    let DeviceRequest { device, body } = request;
    let wake = state
        .db
        .call(move |c| -> ApiResult<Option<String>> {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT state, summary FROM commands WHERE id = ?1 AND device_id = ?2",
                    params![id, device.id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            let (current, summary) = row.ok_or_else(|| ApiError::not_found("no such request"))?;
            if ["done", "failed", "denied", "expired"].contains(&current.as_str()) {
                return Err(ApiError::conflict("that request is finished"));
            }
            let mut failed = body.failed.clone();
            failed.truncate(200);
            let detail: String = body.detail.chars().take(2000).collect();
            c.execute(
                "UPDATE commands SET state = ?1, detail = ?2, approval = ?3, freed = ?4, failed = ?5,
                                     updated = ?6 WHERE id = ?7",
                params![
                    state_name(body.state),
                    detail,
                    body.approval.as_ref().map(|a| serde_json::to_string(a).unwrap_or_default()),
                    body.freed as i64,
                    serde_json::to_string(&failed).unwrap_or_default(),
                    now(),
                    id
                ],
            )?;
            let mut wake = None;
            if body.state == CommandState::NeedsApprover {
                let approver = device
                    .info
                    .approver
                    .clone()
                    .ok_or_else(|| ApiError::bad_request("this device has no approver set"))?;
                // Asked again (after a restart): a decision already made is
                // handed over again.
                c.execute(
                    "INSERT INTO approvals (command_id, approver, asked) VALUES (?1, ?2, ?3)
                     ON CONFLICT (command_id) DO UPDATE SET decision_delivered = NULL",
                    params![id, approver, now()],
                )?;
                wake = c
                    .query_row(
                        "SELECT id FROM devices WHERE fingerprint = ?1 AND removed = 0",
                        [&approver],
                        |r| r.get(0),
                    )
                    .optional()?;
            }
            let action = match body.state {
                CommandState::Shown => None,
                CommandState::NeedsApprover => Some("waiting for the approver"),
                CommandState::Running => Some("request approved"),
                CommandState::Denied => Some("request denied"),
                CommandState::Expired => Some("request expired"),
                CommandState::Done => Some("request done"),
                CommandState::Failed => Some("request failed"),
            };
            if let Some(action) = action {
                audit::append(
                    c,
                    &device.name,
                    action,
                    Some(&device.id),
                    &json!({"request": id, "summary": summary, "detail": detail,
                            "approval": body.approval, "freed": body.freed, "failed": failed}),
                )?;
            }
            Ok(wake)
        })
        .await?;
    if let Some(approver) = wake {
        state.wakers.wake(&approver);
    }
    Ok(Json(json!({"ok": true})))
}

/// `POST /api/v1/decisions`: the designated approver's answer, signed by
/// the approver's device. Passed on to the device the request is for, which
/// checks the signature itself.
pub async fn decide(
    State(state): State<AppState>,
    request: DeviceRequest<Signed<Decision>>,
) -> ApiResult<Json<Value>> {
    let DeviceRequest {
        device,
        body: signed,
    } = request;
    let key: String = {
        let id = device.id.clone();
        state
            .db
            .call(move |c| c.query_row("SELECT key FROM devices WHERE id = ?1", [id], |r| r.get(0)))
            .await?
    };
    let key =
        PublicKey::from_base64(&key).ok_or_else(|| ApiError::unauthorized("unknown device"))?;
    let decision = signed
        .open(&key, Purpose::Decision)
        .map_err(|e| ApiError::bad_request(format!("decision: {e}")))?;
    let target = state
        .db
        .call(move |c| -> ApiResult<String> {
            let row: Option<(String, String, String, String, Option<String>)> = c
                .query_row(
                    "SELECT m.device_id, m.signed, m.summary, a.approver, a.decision FROM commands m
                     JOIN approvals a ON a.command_id = m.id
                     WHERE m.id = ?1 AND m.state = 'needs_approver'",
                    [&decision.command],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
                )
                .optional()?;
            let (target, command, summary, approver, earlier) =
                row.ok_or_else(|| ApiError::not_found("no request is waiting for that decision"))?;
            if approver != device.fingerprint {
                return Err(ApiError::forbidden(
                    "this device isn't that request's approver",
                ));
            }
            if earlier.is_some() {
                return Err(ApiError::conflict("already decided"));
            }
            let command: Signed<Command> =
                serde_json::from_str(&command).map_err(|e| ApiError::bad_request(e.to_string()))?;
            if command.hash() != decision.command_hash || decision.device != target {
                return Err(ApiError::bad_request(
                    "the decision is about a different request",
                ));
            }
            c.execute(
                "UPDATE approvals SET decision = ?1, decided = ?2 WHERE command_id = ?3",
                params![
                    serde_json::to_string(&signed).unwrap_or_default(),
                    now(),
                    decision.command
                ],
            )?;
            audit::append(
                c,
                &device.name,
                if decision.allow {
                    "approver allowed"
                } else {
                    "approver denied"
                },
                Some(&target),
                &json!({"request": decision.command, "summary": summary,
                        "grant_minutes": decision.minutes}),
            )?;
            Ok(target)
        })
        .await?;
    state.wakers.wake(&target);
    Ok(Json(json!({"ok": true})))
}
