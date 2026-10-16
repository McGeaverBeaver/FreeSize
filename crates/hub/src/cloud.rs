//! What the FreeSize Nextcloud app reports (0.20): per covered user, the
//! biggest files, files nobody changed in a year, files whose contents are
//! the same (by SHA-256, read on the Nextcloud server), and what the trash
//! bin and old versions take.
//!
//! Each person sees only the Nextcloud account with their email address;
//! admins see every account. Deletes go to the app, which moves the files
//! into that user's trash bin once the approver allowed it on their phone.

use std::collections::HashSet;

use freesize_proto::cloud::{CloudReport, CloudUser};
use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{Value, json};

use crate::auth::User;
use crate::copies::Suggestion;
use crate::dashboard::DeviceSummary;
use crate::error::{ApiError, ApiResult};

/// Trash and old versions worth pointing at.
const TRASH_WORTH: u64 = 100 * 1024 * 1024;
/// Most paths in one request, as the Hub takes them.
const MAX_PATHS: usize = 500;

/// One Nextcloud with the app, and the accounts this person may see.
#[derive(Clone, Debug, Serialize)]
pub struct CloudView {
    pub device_id: String,
    pub device_name: String,
    pub server: String,
    pub version: String,
    pub taken: i64,
    pub trash_bin: bool,
    /// Requests can be sent: the app has an approver and the person is an
    /// admin.
    pub can_delete: bool,
    /// The app has an approver.
    pub approver: bool,
    pub users: Vec<CloudUser>,
}

/// Whether `user` may see `account`: admins all, everyone else their own,
/// matched by email.
pub fn sees(user: &User, account: &CloudUser) -> bool {
    user.admin
        || (!user.email.trim().is_empty()
            && account.email.trim().eq_ignore_ascii_case(user.email.trim()))
}

pub fn report(c: &Connection, device: &str) -> rusqlite::Result<Option<CloudReport>> {
    let text: Option<String> = c
        .query_row(
            "SELECT report FROM cloud WHERE device_id = ?1",
            [device],
            |r| r.get(0),
        )
        .optional()?;
    Ok(text.and_then(|t| serde_json::from_str(&t).ok()))
}

/// The views of the visible Nextcloud devices, with only the accounts
/// `user` may see.
pub fn views(
    c: &Connection,
    devices: &[DeviceSummary],
    user: &User,
) -> rusqlite::Result<Vec<CloudView>> {
    let mut out = Vec::new();
    for device in devices {
        let Some(report) = report(c, &device.id)? else {
            continue;
        };
        if let Some(view) = view(device, report, user) {
            out.push(view);
        }
    }
    Ok(out)
}

pub fn view(device: &DeviceSummary, report: CloudReport, user: &User) -> Option<CloudView> {
    let users: Vec<CloudUser> = report
        .users
        .into_iter()
        .filter(|account| sees(user, account))
        .collect();
    if users.is_empty() && !user.admin {
        return None;
    }
    Some(CloudView {
        device_id: device.id.clone(),
        device_name: device.name.clone(),
        server: report.server,
        version: report.version,
        taken: report.taken,
        trash_bin: report.trash_bin,
        can_delete: user.admin && report.trash_bin && device.info.approver.is_some(),
        approver: device.info.approver.is_some(),
        users,
    })
}

/// What could be cleaned up in each account, as the Copies page lists it.
pub fn suggestions(views: &[CloudView]) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for view in views {
        for account in &view.users {
            let whose = format!("{}'s Nextcloud", account.name);
            let copies: usize = account
                .duplicates
                .iter()
                .map(|d| d.files.len().saturating_sub(1))
                .sum();
            if copies > 0 {
                let paths: Vec<String> = account
                    .duplicates
                    .iter()
                    .flat_map(|d| d.files.iter().skip(1).map(|f| f.path.clone()))
                    .take(MAX_PATHS)
                    .collect();
                let bytes = account.duplicate_bytes();
                let mut detail = format!(
                    "{} file{} in {whose} {} the same, byte for byte, as another file there \
                     (checked by a SHA-256 of the contents on the server). The oldest copy of each \
                     stays; the others go to the trash bin, where {} can restore them.",
                    thousands(copies as u64),
                    if copies == 1 { "" } else { "s" },
                    if copies == 1 { "is" } else { "are" },
                    account.name
                );
                if account.hashed < account.to_hash {
                    detail.push_str(&format!(
                        " Still checking: {} of {} files of the same size so far.",
                        thousands(account.hashed),
                        thousands(account.to_hash)
                    ));
                }
                out.push(Suggestion {
                    kind: "cloud_duplicates".into(),
                    title: format!("Extra copies in {whose}"),
                    detail,
                    device_id: view.device_id.clone(),
                    device_name: view.device_name.clone(),
                    bytes,
                    action: view.can_delete.then(|| {
                        json!({"type": "delete", "root": account.id, "paths": paths, "bytes": bytes})
                    }),
                    pointer: pointer(view),
                });
            }
            if account.old_files > 0 {
                out.push(Suggestion {
                    kind: "cloud_old".into(),
                    title: format!("Files nobody changed in a year, in {whose}"),
                    detail: format!(
                        "{} file{} not changed in over a year. Old isn't the same as unwanted: \
                         look through them and tick what can go.",
                        thousands(account.old_files),
                        if account.old_files == 1 { "" } else { "s" }
                    ),
                    device_id: view.device_id.clone(),
                    device_name: view.device_name.clone(),
                    bytes: account.old_bytes,
                    action: None,
                    pointer: "Choose which on the In Nextcloud tab.".into(),
                });
            }
            let held = account.trash + account.versions;
            if held >= TRASH_WORTH {
                out.push(Suggestion {
                    kind: "cloud_trash".into(),
                    title: format!("Trash bin and old versions in {whose}"),
                    detail: format!(
                        "{} file{} in the trash bin and {} old version{} of files. Nextcloud \
                         empties both by its own rules; {} can empty Deleted files sooner, and \
                         the admin can keep versions for less time.",
                        thousands(account.trash_files),
                        if account.trash_files == 1 { "" } else { "s" },
                        thousands(account.versions_files),
                        if account.versions_files == 1 { "" } else { "s" },
                        account.name
                    ),
                    device_id: view.device_id.clone(),
                    device_name: view.device_name.clone(),
                    bytes: held,
                    action: None,
                    pointer: "Empty it in Nextcloud, under Files, Deleted files.".into(),
                });
            }
        }
    }
    out
}

/// Without the app, what the Hub's own listing of a connected account
/// shows for certain: big files not changed in a year, and the trash bin.
/// Nothing is guessed: no file is called a copy of another here.
pub fn listing_suggestions(
    listings: &[crate::copies::ServerListing],
    views: &[CloudView],
    now: i64,
) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for listing in listings {
        let account = &listing.account;
        if covered_by(views, &account.server, &account.user).is_some() {
            continue;
        }
        let whose = format!("{} on {}", account.user, account.server);
        let old: Vec<_> = listing
            .entries
            .iter()
            .filter(|e| {
                !e.is_dir && e.modified > 0 && e.modified < now - freesize_proto::cloud::OLD_AFTER
            })
            .collect();
        if !old.is_empty() {
            out.push(Suggestion {
                kind: "cloud_old".into(),
                title: format!("Big files nobody changed in a year, for {whose}"),
                detail: format!(
                    "{} big file{} the Hub listed {} not changed in over a year. Old isn't the \
                     same as unwanted: look through them in Nextcloud.",
                    thousands(old.len() as u64),
                    if old.len() == 1 { "" } else { "s" },
                    if old.len() == 1 { "was" } else { "were" }
                ),
                bytes: old.iter().map(|e| e.size).sum(),
                pointer: "The FreeSize Nextcloud app lists every file, and can move them to the \
                          trash bin once the approver allows it."
                    .into(),
                ..Suggestion::default()
            });
        }
        if account.trash > 0 && account.trash as u64 >= TRASH_WORTH {
            out.push(Suggestion {
                kind: "cloud_trash".into(),
                title: format!("Trash bin for {whose}"),
                detail: "Nextcloud empties it by its own rules; it can be emptied sooner.".into(),
                bytes: account.trash as u64,
                pointer: "Empty it in Nextcloud, under Files, Deleted files.".into(),
                ..Suggestion::default()
            });
        }
    }
    out
}

/// Whether two addresses name the same Nextcloud: the Hub signs in through
/// the public address, the app reports what Nextcloud has for itself, which
/// may differ in scheme or case. An app that reports no address matches any.
fn same_server(a: &str, b: &str) -> bool {
    let host = |s: &str| {
        s.trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_ascii_lowercase()
    };
    let (a, b) = (host(a), host(b));
    a.is_empty() || b.is_empty() || a == b
}

/// The covered user behind a Nextcloud account the Hub is signed in to:
/// the account's name is the user's id, or their email.
pub fn account_for<'a>(
    server: &str,
    users: &'a [CloudUser],
    account_server: &str,
    account_user: &str,
) -> Option<&'a CloudUser> {
    if !same_server(server, account_server) {
        return None;
    }
    let account_user = account_user.trim();
    users.iter().find(|u| {
        u.id == account_user
            || (!u.email.trim().is_empty() && u.email.trim().eq_ignore_ascii_case(account_user))
    })
}

/// The app device and user id that cover a Hub-connected account, if the
/// person may ask it for deletes.
pub fn covered_by<'a>(
    views: &'a [CloudView],
    account_server: &str,
    account_user: &str,
) -> Option<(&'a CloudView, &'a CloudUser)> {
    views.iter().find_map(|v| {
        account_for(&v.server, &v.users, account_server, account_user).map(|u| (v, u))
    })
}

fn pointer(view: &CloudView) -> String {
    if !view.trash_bin {
        "Turn on Nextcloud's trash bin (the Deleted files app) first.".into()
    } else if !view.approver {
        "Choose the approver in Nextcloud's admin settings, under FreeSize.".into()
    } else if view.can_delete {
        String::new()
    } else {
        "An admin of the Hub can ask Nextcloud to move them to the trash bin.".into()
    }
}

/// `6,200`.
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The size of a delete the dashboard asks of a Nextcloud with the app:
/// every path must be a file or folder its newest report listed for that
/// user, or one the Hub's own listing of that user's account holds (the
/// Copies page's big files). The app checks each one again before anyone
/// is asked.
pub fn delete_size(c: &Connection, device: &str, user: &str, paths: &[String]) -> ApiResult<u64> {
    let report = report(c, device)?
        .ok_or_else(|| ApiError::bad_request("the Nextcloud app hasn't reported yet"))?;
    let account =
        report.users.iter().find(|u| u.id == user).ok_or_else(|| {
            ApiError::bad_request(format!("the app doesn't cover the user {user}"))
        })?;
    if !report.trash_bin {
        return Err(ApiError::conflict(
            "Nextcloud's trash bin is off, so the app deletes nothing",
        ));
    }
    // The Hub's listings of this user's account, by WebDAV.
    let listed_ids: Vec<i64> = crate::nextcloud::accounts(c)?
        .iter()
        .filter(|a| {
            account_for(
                &report.server,
                std::slice::from_ref(account),
                &a.server,
                &a.user,
            )
            .is_some()
        })
        .map(|a| a.id)
        .collect();
    let mut from_listing = c.prepare(
        "SELECT size FROM nextcloud_files WHERE account_id = ?1 AND path = ?2 AND size >= 0",
    )?;
    let mut seen = HashSet::new();
    let mut total = 0;
    for path in paths {
        if !seen.insert(path.as_str()) {
            continue;
        }
        let size = account
            .listed()
            .find(|f| &f.path == path)
            .map(|f| f.size)
            .or_else(|| {
                account
                    .folders
                    .iter()
                    .find(|f| &f.path == path)
                    .map(|f| f.size)
            })
            .or_else(|| {
                listed_ids.iter().find_map(|id| {
                    from_listing
                        .query_row((id, format!("/{path}")), |r| r.get::<_, i64>(0))
                        .optional()
                        .ok()
                        .flatten()
                        .map(|s| s as u64)
                })
            })
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "{path} isn't in the app's newest report for {}",
                    account.name
                ))
            })?;
        total += size;
    }
    Ok(total)
}

/// A Nextcloud device's report for the device page, cut to what `user`
/// may see.
pub fn for_device(
    c: &Connection,
    device: &DeviceSummary,
    user: &User,
) -> rusqlite::Result<Option<Value>> {
    Ok(report(c, &device.id)?
        .and_then(|r| view(device, r, user))
        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null)))
}

#[cfg(test)]
mod tests {
    use crate::copies::ServerListing;
    use crate::nextcloud::{Account, Entry};

    fn listing(user: &str, trash: i64) -> ServerListing {
        let entry = |path: &str, modified: i64| Entry {
            path: path.into(),
            fileid: 1,
            size: 200 * 1024 * 1024,
            is_dir: false,
            modified,
            checksums: vec![],
        };
        ServerListing {
            account: Account {
                id: 1,
                server: "https://cloud.example.com".into(),
                user: user.into(),
                added: 1,
                added_by: "Admin".into(),
                synced: Some(1),
                error: None,
                files: 2,
                folders: 0,
                bytes: 0,
                trash,
                quota_used: 0,
                quota_free: -1,
                syncing: false,
            },
            entries: vec![
                entry("/old.iso", 1_000_000_000),
                entry("/new.iso", 1_800_000_000),
            ],
        }
    }

    #[test]
    fn without_the_app_only_what_is_certain() {
        let found =
            super::listing_suggestions(&[listing("bob", 500 * 1024 * 1024)], &[], 1_800_000_000);
        let kinds: Vec<&str> = found.iter().map(|s| s.kind.as_str()).collect();
        assert_eq!(kinds, ["cloud_old", "cloud_trash"]);
        assert_eq!(found[0].bytes, 200 * 1024 * 1024);
        assert!(found.iter().all(|s| s.action.is_none()));
    }

    #[test]
    fn the_app_covers_the_account() {
        let view = super::CloudView {
            device_id: "d".into(),
            device_name: "Nextcloud".into(),
            server: "https://CLOUD.example.com/".into(),
            version: "34".into(),
            taken: 1,
            trash_bin: true,
            can_delete: true,
            approver: true,
            users: vec![freesize_proto::cloud::CloudUser {
                id: "bob".into(),
                ..Default::default()
            }],
        };
        let found = super::listing_suggestions(
            &[listing("bob", 500 * 1024 * 1024)],
            &[view],
            1_800_000_000,
        );
        assert!(found.is_empty());
    }

    #[test]
    fn the_account_names_the_user_or_their_email() {
        let users = vec![
            freesize_proto::cloud::CloudUser {
                id: "bob".into(),
                email: "Bob@Example.lan".into(),
                ..Default::default()
            },
            freesize_proto::cloud::CloudUser {
                id: "carol".into(),
                ..Default::default()
            },
        ];
        let found = |server: &str, user: &str| {
            super::account_for("https://cloud.example.lan", &users, server, user)
                .map(|u| u.id.as_str())
        };
        assert_eq!(found("https://cloud.example.lan", "bob"), Some("bob"));
        assert_eq!(
            found("http://CLOUD.example.lan/", "bob@example.lan"),
            Some("bob")
        );
        assert_eq!(found("", "carol"), Some("carol"));
        assert_eq!(found("https://other.example.lan", "bob"), None);
        assert_eq!(found("https://cloud.example.lan", "dave"), None);
        // A user without an email isn't matched by an empty name.
        assert_eq!(found("https://cloud.example.lan", ""), None);
    }

    #[test]
    fn thousands() {
        assert_eq!(super::thousands(0), "0");
        assert_eq!(super::thousands(999), "999");
        assert_eq!(super::thousands(6200), "6,200");
        assert_eq!(super::thousands(1_234_567), "1,234,567");
    }
}
