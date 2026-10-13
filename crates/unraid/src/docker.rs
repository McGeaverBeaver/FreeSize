//! Docker on Unraid: what the Engine says over its socket (containers,
//! images, volumes, build cache), the dockerMan templates of installed and
//! removed apps, and the appdata folders, each sorted into safe, check
//! first, or protected.
//!
//! - **Safe**: dangling images (left by updates) and build cache.
//! - **Check first**: images no container uses (they can be pulled again),
//!   volumes nothing mounts, appdata folders of apps that are gone (which
//!   you may want back from Apps → Previous Apps), big logs and containers
//!   writing inside themselves (both fixed in settings, not by deleting).
//! - **Protected**: whatever an existing container (running or stopped)
//!   uses.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::time::Duration;

use freesize_core::format_size;
use freesize_proto::unraid::{DockerReport, Finding, Safety};
use serde_json::Value;

use crate::System;
use crate::array::Array;
use crate::ini::parse_cfg;
use crate::scans::Scans;

/// Logs this big are worth a look.
const BIG_LOG: u64 = 100 * 1024 * 1024;
/// Containers writing this much inside themselves are worth a look.
const BIG_LAYER: u64 = 1024 * 1024 * 1024;

/// Docker's settings from `/boot/config/docker.cfg`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockerConfig {
    pub enabled: bool,
    /// `image` (docker.img) or `folder`.
    pub mode: String,
    /// docker.img, or the folder in directory mode.
    pub location: String,
    /// Where app settings go, e.g. `/mnt/user/appdata/`.
    pub appdata: String,
}

impl DockerConfig {
    /// The share holding app settings, e.g. `appdata`.
    pub fn appdata_share(&self) -> String {
        share_of(&self.appdata).unwrap_or_else(|| "appdata".into())
    }
}

/// The share a `/mnt/<disk>/<share>/...` path is in.
pub fn share_of(path: &str) -> Option<String> {
    let mut parts = path.trim_start_matches('/').split('/');
    (parts.next()? == "mnt").then_some(())?;
    parts.next()?;
    parts.next().filter(|s| !s.is_empty()).map(String::from)
}

pub fn config(system: &System) -> DockerConfig {
    let cfg = system
        .read("/boot/config/docker.cfg")
        .map(|t| parse_cfg(&t))
        .unwrap_or_default();
    let get = |k: &str, default: &str| {
        cfg.get(k)
            .filter(|v| !v.is_empty())
            .cloned()
            .unwrap_or_else(|| default.into())
    };
    let folder = get("DOCKER_IMAGE_TYPE", "") == "folder";
    DockerConfig {
        enabled: get("DOCKER_ENABLED", "no") == "yes",
        mode: if folder { "folder" } else { "image" }.into(),
        location: get(
            "DOCKER_IMAGE_FILE",
            if folder {
                "/mnt/user/system/docker/docker"
            } else {
                "/mnt/user/system/docker/docker.img"
            },
        ),
        appdata: get("DOCKER_APP_CONFIG_PATH", "/mnt/user/appdata/"),
    }
}

/// The Docker Engine's API on its Unix socket.
pub struct Engine {
    socket: PathBuf,
    timeout: Duration,
}

impl Engine {
    pub fn new(system: &System) -> Engine {
        Engine {
            socket: system.file("/var/run/docker.sock"),
            timeout: Duration::from_secs(120),
        }
    }

    pub fn available(&self) -> bool {
        self.socket.exists()
    }

    #[cfg(unix)]
    fn call(&self, method: &str, path: &str) -> Result<(u16, Vec<u8>), String> {
        let mut stream = std::os::unix::net::UnixStream::connect(&self.socket)
            .map_err(|e| format!("Docker isn't answering: {e}"))?;
        stream
            .set_read_timeout(Some(self.timeout))
            .and_then(|()| stream.set_write_timeout(Some(self.timeout)))
            .map_err(|e| e.to_string())?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: docker\r\nUser-Agent: FreeSize\r\n\
             Content-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut raw = Vec::new();
        stream
            .take(256 * 1024 * 1024)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        parse_response(&raw)
    }

    #[cfg(not(unix))]
    fn call(&self, _method: &str, _path: &str) -> Result<(u16, Vec<u8>), String> {
        Err("Docker's socket is only on Unix".into())
    }

    fn json(&self, method: &str, path: &str) -> Result<Value, String> {
        let (status, body) = self.call(method, path)?;
        let value: Value = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap_or(Value::Null)
        };
        if (200..300).contains(&status) {
            Ok(value)
        } else {
            Err(value["message"]
                .as_str()
                .map_or_else(|| format!("Docker answered {status}"), String::from))
        }
    }

    pub fn get(&self, path: &str) -> Result<Value, String> {
        self.json("GET", path)
    }

    pub fn delete(&self, path: &str) -> Result<Value, String> {
        self.json("DELETE", path)
    }

    pub fn post(&self, path: &str) -> Result<Value, String> {
        self.json("POST", path)
    }
}

/// An HTTP/1.1 response: its status and body, with chunked encoding undone.
fn parse_response(raw: &[u8]) -> Result<(u16, Vec<u8>), String> {
    let split = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("Docker's answer was cut off")?;
    let head = String::from_utf8_lossy(&raw[..split]);
    let status = head
        .lines()
        .next()
        .and_then(|line| line.split(' ').nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or("Docker's answer had no status")?;
    let header = |name: &str| {
        head.lines().skip(1).find_map(|line| {
            let (key, value) = line.split_once(':')?;
            key.trim()
                .eq_ignore_ascii_case(name)
                .then(|| value.trim().to_ascii_lowercase())
        })
    };
    let body = &raw[split + 4..];
    if header("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
        return Ok((status, dechunk(body)?));
    }
    let body = match header("content-length").and_then(|v| v.parse::<usize>().ok()) {
        Some(len) => &body[..len.min(body.len())],
        None => body,
    };
    Ok((status, body.to_vec()))
}

fn dechunk(mut body: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    loop {
        let end = body
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("Docker's answer was cut off")?;
        let size_text = String::from_utf8_lossy(&body[..end]);
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| "Docker's answer was garbled")?;
        body = &body[end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size {
            return Err("Docker's answer was cut off".into());
        }
        out.extend_from_slice(&body[..size]);
        body = body.get(size + 2..).unwrap_or_default();
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Container {
    pub id: String,
    pub name: String,
    pub image_id: String,
    pub image: String,
    pub running: bool,
    /// Host paths it mounts.
    pub mounts: Vec<String>,
    /// The same with where each is inside the container (host, inside).
    pub binds: Vec<(String, String)>,
    /// What it wrote inside itself.
    pub size_rw: u64,
    pub log_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Image {
    pub id: String,
    pub tags: Vec<String>,
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Volume {
    pub name: String,
    pub size: Option<u64>,
    pub refs: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inventory {
    pub containers: Vec<Container>,
    pub images: Vec<Image>,
    pub volumes: Vec<Volume>,
    /// Build cache nothing uses.
    pub build_cache: u64,
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_string()
}

fn bytes(value: &Value) -> u64 {
    value.as_i64().filter(|&v| v > 0).unwrap_or(0) as u64
}

/// Asks the Engine what it has.
pub fn inventory(engine: &Engine, system: &System) -> Result<Inventory, String> {
    let df = engine.get("/system/df")?;
    let listed = engine.get("/containers/json?all=1")?;
    let size_rw: BTreeMap<String, u64> = df["Containers"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|c| (text(&c["Id"]), bytes(&c["SizeRw"])))
                .collect()
        })
        .unwrap_or_default();
    let mut containers = Vec::new();
    for c in listed.as_array().map(Vec::as_slice).unwrap_or_default() {
        let id = text(&c["Id"]);
        let name = c["Names"][0]
            .as_str()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_string();
        let mounts: Vec<String> = c["Mounts"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|m| m["Source"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        let binds: Vec<(String, String)> = c["Mounts"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|m| {
                        Some((
                            m["Source"].as_str()?.to_string(),
                            m["Destination"].as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The log's size, from the file Docker names.
        let log_bytes = engine
            .get(&format!("/containers/{id}/json"))
            .ok()
            .and_then(|detail| detail["LogPath"].as_str().map(String::from))
            .filter(|p| !p.is_empty())
            .and_then(|p| std::fs::metadata(system.file(&p)).ok())
            .map_or(0, |m| m.len());
        containers.push(Container {
            size_rw: size_rw.get(&id).copied().unwrap_or(0),
            image_id: text(&c["ImageID"]),
            image: text(&c["Image"]),
            running: c["State"].as_str() == Some("running"),
            id,
            name,
            mounts,
            binds,
            log_bytes,
        });
    }
    let images = df["Images"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|i| Image {
                    id: text(&i["Id"]),
                    tags: i["RepoTags"]
                        .as_array()
                        .map(|tags| {
                            tags.iter()
                                .filter_map(|t| t.as_str())
                                .filter(|t| *t != "<none>:<none>")
                                .map(String::from)
                                .collect()
                        })
                        .unwrap_or_default(),
                    size: bytes(&i["Size"]),
                })
                .collect()
        })
        .unwrap_or_default();
    let volumes = df["Volumes"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|v| Volume {
                    name: text(&v["Name"]),
                    size: v["UsageData"]["Size"]
                        .as_i64()
                        .filter(|&s| s >= 0)
                        .map(|s| s as u64),
                    refs: v["UsageData"]["RefCount"].as_i64().unwrap_or(-1),
                })
                .collect()
        })
        .unwrap_or_default();
    let build_cache = df["BuildCache"]
        .as_array()
        .map(|list| {
            list.iter()
                .filter(|b| !b["InUse"].as_bool().unwrap_or(false))
                .map(|b| bytes(&b["Size"]))
                .sum()
        })
        .unwrap_or(0);
    Ok(Inventory {
        containers,
        images,
        volumes,
        build_cache,
    })
}

/// An app's dockerMan template: from installing it, and kept after it's
/// removed so Apps → Previous Apps can put it back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Template {
    pub name: String,
    /// Host paths it maps.
    pub paths: Vec<String>,
}

pub fn templates(system: &System) -> Vec<Template> {
    let dir = system.file("/boot/config/plugins/dockerMan/templates-user");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut list: Vec<Template> = entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "xml"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|xml| parse_template(&xml))
        .collect();
    list.sort_by(|a, b| a.name.cmp(&b.name));
    list
}

fn parse_template(xml: &str) -> Option<Template> {
    let doc = roxmltree::Document::parse(xml).ok()?;
    let root = doc.root_element();
    let name = root
        .children()
        .find(|n| n.has_tag_name("Name"))?
        .text()?
        .trim()
        .to_string();
    let mut paths = Vec::new();
    for node in root.descendants() {
        let path = if node.has_tag_name("Config") && node.attribute("Type") == Some("Path") {
            node.text()
                .filter(|t| !t.trim().is_empty())
                .or_else(|| node.attribute("Default"))
        } else if node.has_tag_name("HostDir") {
            node.text()
        } else {
            None
        };
        if let Some(path) = path.map(str::trim).filter(|p| p.starts_with('/')) {
            paths.push(path.to_string());
        }
    }
    Some(Template { name, paths })
}

/// The appdata folder a host path is in, e.g. `plex` for
/// `/mnt/cache/appdata/plex/Library`.
pub fn appdata_folder(path: &str, share: &str) -> Option<String> {
    let mut parts = path.trim_start_matches('/').split('/');
    (parts.next()? == "mnt").then_some(())?;
    parts.next()?;
    (parts.next()? == share).then_some(())?;
    parts.next().filter(|n| !n.is_empty()).map(String::from)
}

/// Paths nothing may delete because of Docker: the image or folder, every
/// appdata folder an existing container uses (with all that's in it), and
/// every other folder a container mounts (itself, not what's in it).
pub fn protected_paths(
    config: &DockerConfig,
    inventory: Option<&Inventory>,
) -> Vec<(String, String, bool)> {
    let mut list = vec![(
        config.location.trim_end_matches('/').to_string(),
        "Docker's own storage".to_string(),
        true,
    )];
    let share = config.appdata_share();
    let Some(inventory) = inventory else {
        // Docker is off: which app uses what is unknown, so all of it.
        list.push((
            format!("/mnt/user/{share}"),
            "app settings (Docker isn't running, so which app uses what can't be told)".into(),
            true,
        ));
        return list;
    };
    for container in &inventory.containers {
        for mount in &container.mounts {
            match appdata_folder(mount, &share) {
                Some(folder) => list.push((
                    format!("/mnt/user/{share}/{folder}"),
                    format!("the settings of {}", container.name),
                    true,
                )),
                None => list.push((
                    mount.trim_end_matches('/').to_string(),
                    format!("mounted into {}", container.name),
                    false,
                )),
            }
        }
    }
    list
}

/// Sorts what Docker has into findings.
pub fn report(
    system: &System,
    config: &DockerConfig,
    inventory: Result<Inventory, String>,
    templates: &[Template],
    array: &Array,
    scans: &Scans,
) -> DockerReport {
    let mut report = DockerReport {
        mode: config.mode.clone(),
        location: config.location.clone(),
        ..DockerReport::default()
    };
    if config.mode == "image" {
        report.image_size = std::fs::metadata(system.file(&config.location)).map_or(0, |m| m.len());
        report.image_used = used_space(system, "/var/lib/docker");
    }
    let share = config.appdata_share();
    let inventory = match inventory {
        Ok(inventory) => {
            report.running = true;
            inventory
        }
        // Docker is off: appdata can still be sorted by the templates, but
        // nothing can be called unused.
        Err(_) => Inventory::default(),
    };
    report.containers = inventory.containers.len() as u32;
    report.running_containers = inventory.containers.iter().filter(|c| c.running).count() as u32;
    let mut findings = Vec::new();

    // Images.
    let mut used_images: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for container in &inventory.containers {
        used_images
            .entry(container.image_id.as_str())
            .or_default()
            .push(container.name.as_str());
    }
    for image in &inventory.images {
        let users = used_images.get(image.id.as_str());
        let label = image
            .tags
            .first()
            .cloned()
            .unwrap_or_else(|| format!("untagged image {}", short(&image.id)));
        let (kind, safety, reason) = match users {
            Some(names) => (
                "image",
                Safety::Protected,
                format!("used by {}", names.join(", ")),
            ),
            None if image.tags.is_empty() => (
                "dangling_image",
                Safety::Safe,
                "left behind by an update; no container uses it".into(),
            ),
            None => (
                "image",
                Safety::Check,
                "no container uses it; it can be downloaded again".into(),
            ),
        };
        findings.push(Finding {
            id: format!("image:{}", image.id),
            kind: kind.into(),
            label,
            path: None,
            bytes: image.size,
            safety,
            reason,
        });
    }
    if inventory.build_cache > 0 {
        findings.push(Finding {
            id: "build_cache".into(),
            kind: "build_cache".into(),
            label: "Build cache".into(),
            path: None,
            bytes: inventory.build_cache,
            safety: Safety::Safe,
            reason: "rebuilt when needed; nothing uses it now".into(),
        });
    }

    // Volumes.
    for volume in &inventory.volumes {
        let (safety, reason) = if volume.refs > 0 {
            (Safety::Protected, "a container uses it".to_string())
        } else {
            (
                Safety::Check,
                "no container uses it; it may hold data you want".to_string(),
            )
        };
        findings.push(Finding {
            id: format!("volume:{}", volume.name),
            kind: "volume".into(),
            label: format!("Volume {}", short(&volume.name)),
            path: None,
            bytes: volume.size.unwrap_or(0),
            safety,
            reason,
        });
    }

    // Appdata folders: who uses each.
    let mut users: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
    for container in &inventory.containers {
        for mount in &container.mounts {
            if let Some(folder) = appdata_folder(mount, &share) {
                users
                    .entry(folder)
                    .or_default()
                    .push((container.name.clone(), container.running));
            }
        }
    }
    let installed: BTreeSet<&str> = inventory
        .containers
        .iter()
        .map(|c| c.name.as_str())
        .collect();
    let mut left_by: BTreeMap<String, String> = BTreeMap::new();
    for template in templates {
        for path in &template.paths {
            if let Some(folder) = appdata_folder(path, &share) {
                left_by
                    .entry(folder)
                    .or_insert_with(|| template.name.clone());
            }
        }
    }
    let appdata_root = format!("/mnt/user/{share}");
    for (name, entry) in scans.list(array, &appdata_root) {
        if !entry.dir || name.starts_with('.') {
            continue;
        }
        let (safety, reason) = match users.get(&name) {
            Some(list) => (
                Safety::Protected,
                format!(
                    "used by {}",
                    list.iter()
                        .map(|(n, running)| format!(
                            "{n} ({})",
                            if *running { "running" } else { "stopped" }
                        ))
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ),
            // Without Docker running, which app uses what is unknown: keep
            // everything.
            None if !report.running => (
                Safety::Protected,
                "Docker isn't running, so which app uses it can't be told".into(),
            ),
            None => match left_by.get(&name) {
                Some(app) if !installed.contains(app.as_str()) => (
                    Safety::Check,
                    format!(
                        "left by {app}, which isn't installed; Apps → Previous Apps puts it back \
                         with these settings"
                    ),
                ),
                _ => (Safety::Check, "no container or template uses it".into()),
            },
        };
        findings.push(Finding {
            id: format!("appdata:{name}"),
            kind: "appdata".into(),
            label: format!("{share}/{name}"),
            path: Some(format!("{appdata_root}/{name}")),
            bytes: entry.sizes.allocated,
            safety,
            reason,
        });
    }

    // Logs and containers writing inside themselves: settings to change.
    for container in &inventory.containers {
        if container.log_bytes >= BIG_LOG {
            findings.push(Finding {
                id: format!("log:{}", container.name),
                kind: "log".into(),
                label: format!("{}'s log", container.name),
                path: None,
                bytes: container.log_bytes,
                safety: Safety::Check,
                reason: "turn on log rotation in Settings → Docker (advanced view)".into(),
            });
        }
        if container.size_rw >= BIG_LAYER {
            findings.push(Finding {
                id: format!("layer:{}", container.name),
                kind: "layer".into(),
                label: format!("Inside {}", container.name),
                path: None,
                bytes: container.size_rw,
                safety: Safety::Check,
                reason: format!(
                    "{} writes {} inside the container, which fills docker.img and goes on \
                     every update; map that folder to appdata",
                    container.name,
                    format_size(container.size_rw)
                ),
            });
        }
    }
    findings.sort_by(|a, b| a.safety.cmp(&b.safety).then(b.bytes.cmp(&a.bytes)));
    report.findings = findings;
    report
}

fn short(id: &str) -> String {
    id.trim_start_matches("sha256:").chars().take(12).collect()
}

/// Space in use on the filesystem holding `path`.
pub fn used_space(system: &System, path: &str) -> u64 {
    freesize_core::disk_space(&system.file(path))
        .map_or(0, |space| space.total.saturating_sub(space.free))
}

/// Removes the images and build cache a request names, if they're still
/// unused; returns the bytes freed. Docker itself refuses to remove an
/// image a container uses.
pub fn cleanup(engine: &Engine, inventory: &Inventory, items: &[String]) -> Result<u64, String> {
    let used: BTreeSet<&str> = inventory
        .containers
        .iter()
        .map(|c| c.image_id.as_str())
        .collect();
    let mut freed = 0;
    let mut errors = Vec::new();
    for item in items {
        if item == "build_cache" {
            match engine.post("/build/prune") {
                Ok(answer) => freed += bytes(&answer["SpaceReclaimed"]),
                Err(err) => errors.push(format!("build cache: {err}")),
            }
            continue;
        }
        let Some(id) = item.strip_prefix("image:") else {
            errors.push(format!("{item}: not something this removes"));
            continue;
        };
        if used.contains(id) {
            errors.push(format!("{}: a container uses it now", short(id)));
            continue;
        }
        let Some(image) = inventory.images.iter().find(|i| i.id == id) else {
            errors.push(format!("{}: already gone", short(id)));
            continue;
        };
        // Not forced: Docker refuses if anything still needs it.
        match engine.delete(&format!("/images/{id}")) {
            Ok(_) => freed += image.size,
            Err(err) => errors.push(format!("{}: {err}", short(id))),
        }
    }
    if freed == 0 && !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(freed)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::array::tests::server;
    use crate::scans::tests::scanned;
    use std::sync::{Arc, Mutex};

    /// A stand-in for Docker on the test server's socket. It answers GETs
    /// from `answers` (path → JSON) and records every request.
    #[cfg(unix)]
    pub fn fake_docker(
        system: &System,
        answers: Vec<(&'static str, Value)>,
    ) -> Arc<Mutex<Vec<String>>> {
        use std::io::BufRead;
        let socket = system.file("/var/run/docker.sock");
        std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header == "\r\n" || header.is_empty() {
                        break;
                    }
                }
                let mut parts = line.split(' ');
                let method = parts.next().unwrap_or_default().to_string();
                let path = parts.next().unwrap_or_default().to_string();
                log.lock().unwrap().push(format!("{method} {path}"));
                let found = answers.iter().find(|(p, _)| {
                    let (m, p) = p.split_once(' ').unwrap();
                    m == method && *p == path
                });
                let (status, body) = match found {
                    Some((_, value)) => ("200 OK", value.to_string()),
                    None => (
                        "404 Not Found",
                        r#"{"message":"no such thing"}"#.to_string(),
                    ),
                };
                // Chunked, as Docker sends big answers.
                let half = body.len() / 2;
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Transfer-Encoding: chunked\r\n\r\n{:x}\r\n{}\r\n{:x}\r\n{}\r\n0\r\n\r\n",
                    half,
                    &body[..half],
                    body.len() - half,
                    &body[half..]
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        seen
    }

    pub fn docker_answers() -> Vec<(&'static str, Value)> {
        vec![
            (
                "GET /system/df",
                serde_json::json!({
                    "LayersSize": 9_000_000_000u64,
                    "Images": [
                        {"Id": "sha256:plex1", "RepoTags": ["lscr.io/linuxserver/plex:latest"], "Size": 400_000_000, "Containers": 1},
                        {"Id": "sha256:old1", "RepoTags": ["<none>:<none>"], "Size": 380_000_000, "Containers": 0},
                        {"Id": "sha256:nginx1", "RepoTags": ["nginx:1.25"], "Size": 190_000_000, "Containers": 0}
                    ],
                    "Containers": [
                        {"Id": "c-plex", "SizeRw": 2_500_000_000u64},
                        {"Id": "c-sonarr", "SizeRw": 1_000}
                    ],
                    "Volumes": [
                        {"Name": "8d1f0c33aa", "UsageData": {"Size": 50_000_000, "RefCount": 0}},
                        {"Name": "pgdata", "UsageData": {"Size": 900_000_000, "RefCount": 1}}
                    ],
                    "BuildCache": [
                        {"ID": "b1", "Size": 300_000_000, "InUse": false},
                        {"ID": "b2", "Size": 5_000, "InUse": true}
                    ]
                }),
            ),
            (
                "GET /containers/json?all=1",
                serde_json::json!([
                    {"Id": "c-plex", "Names": ["/plex"], "Image": "lscr.io/linuxserver/plex", "ImageID": "sha256:plex1",
                     "State": "running", "Mounts": [
                        {"Type": "bind", "Source": "/mnt/cache/appdata/plex", "Destination": "/config"},
                        {"Type": "bind", "Source": "/mnt/user/Movies", "Destination": "/movies"}]},
                    {"Id": "c-sonarr", "Names": ["/sonarr"], "Image": "sonarr", "ImageID": "sha256:sonarr1",
                     "State": "exited", "Mounts": [
                        {"Type": "bind", "Source": "/mnt/user/appdata/sonarr", "Destination": "/config"}]}
                ]),
            ),
            (
                "GET /containers/c-plex/json",
                serde_json::json!({"LogPath": "/var/lib/docker/containers/c-plex/c-plex-json.log"}),
            ),
            (
                "GET /containers/c-sonarr/json",
                serde_json::json!({"LogPath": ""}),
            ),
            (
                "DELETE /images/sha256:old1",
                serde_json::json!([{"Deleted": "sha256:old1"}]),
            ),
            (
                "POST /build/prune",
                serde_json::json!({"SpaceReclaimed": 300_000_000}),
            ),
        ]
    }

    fn template(system: &System, file: &str, xml: &str) {
        let path = system.file(&format!(
            "/boot/config/plugins/dockerMan/templates-user/{file}"
        ));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, xml).unwrap();
    }

    #[test]
    fn reads_responses() {
        let (status, body) = parse_response(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2;x=1\r\nde\r\n0\r\n\r\n",
        )
        .unwrap();
        assert_eq!((status, body.as_slice()), (200, &b"abcde"[..]));
        let (status, body) =
            parse_response(b"HTTP/1.1 404 Not Found\r\nContent-Length: 2\r\n\r\n{}garbage")
                .unwrap();
        assert_eq!((status, body.as_slice()), (404, &b"{}"[..]));
        assert!(parse_response(b"HTTP/1.1 200 OK\r\n").is_err());
    }

    #[test]
    fn reads_settings_and_templates() {
        let (_dir, system) = server();
        std::fs::create_dir_all(system.file("/boot/config")).unwrap();
        std::fs::write(
            system.file("/boot/config/docker.cfg"),
            "DOCKER_ENABLED=\"yes\"\nDOCKER_IMAGE_FILE=\"/mnt/user/system/docker/docker\"\n\
             DOCKER_IMAGE_TYPE=\"folder\"\nDOCKER_APP_CONFIG_PATH=\"/mnt/cache/appdata/\"\n",
        )
        .unwrap();
        let cfg = config(&system);
        assert!(cfg.enabled);
        assert_eq!(cfg.mode, "folder");
        assert_eq!(cfg.appdata_share(), "appdata");
        template(
            &system,
            "my-plex.xml",
            r#"<?xml version="1.0"?><Container version="2"><Name>plex</Name>
               <Config Name="Config" Target="/config" Default="/mnt/user/appdata/plex" Type="Path">/mnt/user/appdata/plex</Config>
               <Config Name="Movies" Target="/movies" Default="" Type="Path">/mnt/user/Movies</Config>
               <Config Name="Port" Target="32400" Type="Port">32400</Config></Container>"#,
        );
        template(
            &system,
            "my-old.xml",
            r#"<Container><Name>oldapp</Name><Data><Volume><HostDir>/mnt/user/appdata/oldapp</HostDir></Volume></Data></Container>"#,
        );
        template(&system, "broken.xml", "<Container>");
        let list = templates(&system);
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].name, "plex");
        assert_eq!(
            list[1].paths,
            ["/mnt/user/appdata/plex", "/mnt/user/Movies"]
        );
        assert_eq!(list[0].paths, ["/mnt/user/appdata/oldapp"]);
        assert_eq!(
            appdata_folder("/mnt/cache/appdata/plex/Library", "appdata").as_deref(),
            Some("plex")
        );
        assert_eq!(appdata_folder("/mnt/user/Movies", "appdata"), None);
        assert_eq!(share_of("/mnt/user/appdata/").as_deref(), Some("appdata"));
    }

    #[cfg(unix)]
    #[test]
    fn sorts_docker_into_classes() {
        let (_dir, system) = server();
        let _calls = fake_docker(&system, docker_answers());
        let log = system.file("/var/lib/docker/containers/c-plex/c-plex-json.log");
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::File::create(&log)
            .unwrap()
            .set_len(150 * 1024 * 1024)
            .unwrap();
        template(
            &system,
            "my-oldapp.xml",
            "<Container><Name>oldapp</Name><Config Type=\"Path\">/mnt/user/appdata/oldapp</Config></Container>",
        );
        let (array, scans) = scanned(
            &system,
            &[
                ("/mnt/cache/appdata/plex/db", 40_000),
                ("/mnt/cache/appdata/sonarr/db", 20_000),
                ("/mnt/cache/appdata/oldapp/db", 30_000),
                ("/mnt/disk1/appdata/mystery/x", 10_000),
            ],
        );
        let engine = Engine::new(&system);
        assert!(engine.available());
        let inv = inventory(&engine, &system).unwrap();
        assert_eq!(inv.containers.len(), 2);
        assert_eq!(inv.containers[0].log_bytes, 150 * 1024 * 1024);
        assert_eq!(inv.build_cache, 300_000_000);
        let cfg = config(&system);
        let report = report(
            &system,
            &cfg,
            Ok(inv.clone()),
            &templates(&system),
            &array,
            &scans,
        );
        assert!(report.running);
        assert_eq!((report.containers, report.running_containers), (2, 1));
        let find = |id: &str| {
            report
                .findings
                .iter()
                .find(|f| f.id == id)
                .unwrap_or_else(|| panic!("{id}"))
        };
        assert_eq!(find("image:sha256:plex1").safety, Safety::Protected);
        assert_eq!(find("image:sha256:old1").safety, Safety::Safe);
        assert_eq!(find("image:sha256:old1").kind, "dangling_image");
        assert_eq!(find("image:sha256:nginx1").safety, Safety::Check);
        assert_eq!(find("build_cache").safety, Safety::Safe);
        assert_eq!(find("volume:8d1f0c33aa").safety, Safety::Check);
        assert_eq!(find("volume:pgdata").safety, Safety::Protected);
        let plex = find("appdata:plex");
        assert_eq!(plex.safety, Safety::Protected);
        assert_eq!(plex.reason, "used by plex (running)");
        assert_eq!(find("appdata:sonarr").reason, "used by sonarr (stopped)");
        assert!(find("appdata:oldapp").reason.starts_with("left by oldapp"));
        assert_eq!(
            find("appdata:mystery").reason,
            "no container or template uses it"
        );
        assert_eq!(
            find("appdata:mystery").path.as_deref(),
            Some("/mnt/user/appdata/mystery")
        );
        assert_eq!(find("log:plex").safety, Safety::Check);
        assert_eq!(find("layer:plex").bytes, 2_500_000_000);
        // Safe first.
        assert_eq!(report.findings[0].safety, Safety::Safe);

        let protected = protected_paths(&cfg, Some(&inv));
        assert!(protected.contains(&(
            "/mnt/user/appdata/plex".into(),
            "the settings of plex".into(),
            true
        )));
        assert!(protected.contains(&(
            "/mnt/user/Movies".into(),
            "mounted into plex".into(),
            false
        )));

        // Cleaning removes unused images and build cache, never a used one.
        let freed = cleanup(
            &engine,
            &inv,
            &[
                "image:sha256:old1".into(),
                "build_cache".into(),
                "image:sha256:plex1".into(),
            ],
        )
        .unwrap();
        assert_eq!(freed, 680_000_000);
        assert!(cleanup(&engine, &inv, &["image:sha256:plex1".into()]).is_err());
    }

    #[test]
    fn without_docker_appdata_is_protected() {
        let (_dir, system) = server();
        let (array, scans) = scanned(&system, &[("/mnt/cache/appdata/plex/db", 1_000)]);
        let report = report(
            &system,
            &config(&system),
            Err("off".into()),
            &[],
            &array,
            &scans,
        );
        assert!(!report.running);
        assert_eq!(report.findings.len(), 1);
        assert_eq!(report.findings[0].safety, Safety::Protected);
        let protected = protected_paths(&config(&system), None);
        assert!(
            protected
                .iter()
                .any(|(p, _, all)| p == "/mnt/user/appdata" && *all)
        );
    }
}
