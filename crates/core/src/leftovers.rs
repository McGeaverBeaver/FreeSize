//! Leftovers of removed programs (0.15): the settings and caches a program
//! left in AppData, `~/Library` or the XDG folders after it was
//! uninstalled. A folder is only called a leftover when it's clear whose
//! it was: either FreeSize saw the program installed earlier and matched the
//! folder to it then (kept in `programs.json` in its data folder, see
//! [`Record`]), or the folder is named by the program's own id (a Flatpak id
//! under `~/.var/app`, a snap under `~/snap`, a bundle id under
//! `~/Library/Containers`). The program must be gone now, nothing in the
//! folder may have changed for two weeks, and the list of installed
//! programs must be believable. The result is a "check first" rule of the
//! junk cleaner ([`RULE`]): each line names the program, and the folders go
//! to the quarantine, from where a folder a program still needs comes back.
//!
//! Reading what's installed is per platform ([`installed`]); the matching
//! and the record are the same everywhere and tested with made-up folders.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::junk::{Category, Covers, Env, Finding, Fixture, Item, Platform, Rule, Safety, Take};

/// The rule's id, for `freesize junk --also` and the Hub's Clean request.
pub const ID: &str = "program-leftovers";

/// Nothing in a folder may have changed for this long: a program that's
/// gone stops writing, one that was only renamed or updated doesn't.
pub const AGE_DAYS: u32 = 14;

/// Fewer installed programs than this means the list wasn't read properly
/// (the registry didn't answer, Spotlight is off), and nothing is offered.
const BELIEVABLE: usize = 10;

/// Programs are forgotten this long after they were last seen, unless a
/// folder is still recorded as theirs.
const FORGET_AFTER: i64 = 400 * 86_400;

/// The junk cleaner's rule the leftovers are listed under.
pub static RULE: Rule = Rule {
    id: ID,
    category: Category::Apps,
    title: "Leftovers of removed programs",
    blurb: "Settings and caches of programs that aren't installed any more: FreeSize saw the \
            program installed earlier and matched the folder to it then, or the folder is named \
            by the program's own id, and nothing in it has changed for two weeks. Each line \
            names the program. The folders go to the quarantine, so one a program still needs \
            can be put back.",
    refills: false,
    safety: Safety::CheckFirst,
    app: "",
    processes: &[],
    tool: "",
    patterns: &[],
    take: Take::Whole,
    keep: &[],
    min_age_days: AGE_DAYS,
    fixture: Fixture {
        hit: &[],
        miss: &[],
    },
};

/// An installed program, as far as the folders it owns can be told.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Program {
    /// Stable, prefixed by where it's known from: `flatpak:org.gnome.Maps`,
    /// `snap:spotify`, `desktop:code`, `deb:htop`, `rpm:htop`,
    /// `pacman:htop`, `app:com.spotify.client` (a macOS bundle id) or
    /// `win:{GUID}` (the registry key's name).
    pub id: String,
    /// As shown: "Visual Studio Code".
    pub name: String,
    /// The publisher, where known: "Google LLC".
    #[serde(default)]
    pub vendor: String,
    /// Other names it goes by: a desktop file's stem, its window class, its
    /// executable, a bundle's folder name.
    #[serde(default)]
    pub names: Vec<String>,
    /// Where it's installed, when the system says (Windows' InstallLocation).
    #[serde(default)]
    pub location: Option<PathBuf>,
}

impl Program {
    pub fn new(id: &str, name: &str) -> Program {
        Program {
            id: id.into(),
            name: name.into(),
            ..Program::default()
        }
    }

    /// The keys a folder's name is matched against.
    fn keys(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(self.name.as_str())
            .chain(self.names.iter().map(String::as_str))
            .map(key)
            .filter(|k| usable(k))
    }
}

/// A folder container whose entries are named by a program's own id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strong {
    /// `~/.var/app/<flatpak id>`.
    Flatpak,
    /// `~/snap/<snap>`.
    Snap,
    /// `~/Library/Containers/<bundle id>` and the like.
    Bundle,
}

impl Strong {
    /// The [`Program::id`] a folder of this name would belong to.
    fn id(self, name: &str) -> String {
        match self {
            Strong::Flatpak => format!("flatpak:{name}"),
            Strong::Snap => format!("snap:{name}"),
            Strong::Bundle => format!("app:{}", name.to_lowercase()),
        }
    }

    fn what(self) -> &'static str {
        match self {
            Strong::Flatpak => "Flatpak",
            Strong::Snap => "snap",
            Strong::Bundle => "app",
        }
    }
}

/// What's installed on this machine, and which id-named places were read
/// completely, so a name missing from them means the program is gone.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Installed {
    pub programs: Vec<Program>,
    pub complete: Vec<Strong>,
}

/// What a program was last seen as.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    pub name: String,
    /// Seconds since 1970.
    pub seen: i64,
}

/// What FreeSize has seen: the programs, and which folder belongs to
/// which, as of the looks it took while they were installed. Saved as
/// `programs.json` in the data folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub programs: BTreeMap<String, Seen>,
    /// Folder (or file) path → program id.
    #[serde(default)]
    pub folders: BTreeMap<String, String>,
}

impl Record {
    /// The record at `path`, or an empty one when there's none (or it can't
    /// be read: it's rebuilt over the next looks).
    pub fn load(path: &Path) -> Record {
        fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Where the record lives on this machine: FreeSize's data folder, the one
/// the app and `freesize agent` share. None on Android and Unraid, which
/// have no leftovers of this kind.
pub fn record_path() -> Option<PathBuf> {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let base = if cfg!(target_os = "android") {
        None
    } else if cfg!(windows) {
        var("LOCALAPPDATA").map(|dir| dir.join("FreeSize"))
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|home| home.join("Library/Application Support/FreeSize"))
    } else {
        var("XDG_DATA_HOME")
            .filter(|dir| dir.is_absolute())
            .or_else(|| var("HOME").map(|home| home.join(".local/share")))
            .map(|dir| dir.join("freesize"))
    }?;
    Some(crate::renamed::adopt_dir(base).join("programs.json"))
}

/// One look on this machine: reads what's installed, matches the folders,
/// keeps the record at `record` up to date and answers the finding, if any.
pub fn look(platform: Platform, env: &Env, now: i64, record: Option<&Path>) -> Option<Finding> {
    if !matches!(
        platform,
        Platform::Windows | Platform::MacOs | Platform::Linux
    ) {
        return None;
    }
    let installed = installed(platform);
    let mut seen = record.map(Record::load).unwrap_or_default();
    let before = seen.clone();
    let items = find(platform, env, &installed, &mut seen, now);
    if let Some(path) = record
        && seen != before
        && let Err(err) = seen.save(path)
    {
        eprintln!("freesize: couldn't keep the list of programs: {err}");
    }
    finding(items)
}

/// The rule's finding from its items; none when there are none.
pub fn finding(items: Vec<Item>) -> Option<Finding> {
    (!items.is_empty()).then(|| Finding {
        rule: &RULE,
        bytes: items.iter().map(|i| i.bytes).sum(),
        files: items.iter().map(|i| i.files).sum(),
        running: false,
        items,
    })
}

/// A place whose entries belong to programs.
struct Container {
    dir: PathBuf,
    /// Windows' AppData holds both `Discord` and `Google\Chrome`: the
    /// entries, and their entries as vendor and program.
    two_levels: bool,
    strong: Option<Strong>,
}

/// The places on each platform. Paths are the junk cleaner's [`Env`], so
/// tests build them under one folder.
fn containers(platform: Platform, env: &Env) -> Vec<Container> {
    let place = |dir: PathBuf, two_levels: bool, strong: Option<Strong>| Container {
        dir,
        two_levels,
        strong,
    };
    match platform {
        Platform::Linux => vec![
            place(env.expand("~/.config"), false, None),
            place(env.expand("~/.cache"), false, None),
            place(env.expand("~/.local/share"), false, None),
            place(env.expand("~/.local/state"), false, None),
            place(env.expand("~/.var/app"), false, Some(Strong::Flatpak)),
            place(env.expand("~/snap"), false, Some(Strong::Snap)),
        ],
        Platform::MacOs => vec![
            place(env.expand("~/Library/Application Support"), false, None),
            place(env.expand("~/Library/Caches"), false, Some(Strong::Bundle)),
            place(
                env.expand("~/Library/Containers"),
                false,
                Some(Strong::Bundle),
            ),
            place(env.expand("~/Library/Logs"), false, None),
            place(
                env.expand("~/Library/Saved Application State"),
                false,
                Some(Strong::Bundle),
            ),
            place(
                env.expand("~/Library/Preferences"),
                false,
                Some(Strong::Bundle),
            ),
            place(env.expand("~/Library/WebKit"), false, Some(Strong::Bundle)),
            place(
                env.expand("~/Library/HTTPStorages"),
                false,
                Some(Strong::Bundle),
            ),
        ],
        Platform::Windows => vec![
            // Two levels also reach per-user installs in `Local\Programs`.
            place(env.expand("$LOCAL"), true, None),
            place(env.expand("$ROAMING"), true, None),
            place(env.expand("$PROGRAMDATA"), true, None),
        ],
        Platform::Android | Platform::Unraid => Vec::new(),
    }
}

/// Entries never offered, whatever they match: the system's own, shared
/// between programs, or FreeSize's.
const NEVER: &[&str] = &[
    // Linux
    "trash",
    "fonts",
    "icons",
    "themes",
    "applications",
    "mime",
    "autostart",
    "systemd",
    "dconf",
    "pulse",
    "pipewire",
    "dbus",
    "keyrings",
    "flatpak",
    "snap",
    "environmentd",
    "userdirsdirs",
    "userdirslocale",
    "mimeappslist",
    "gnomeshell",
    "xdgdesktopportal",
    "recentlyusedxbel",
    // Windows
    "microsoft",
    "windows",
    "temp",
    "packages",
    "programs",
    "comms",
    "publishers",
    "d3dscache",
    "connecteddevicesplatform",
    "peernetworking",
    "templates",
    "packagemanagement",
    "programdata",
    // macOS
    "apple",
    "icloud",
    "mobilesync",
    "mobiledocuments",
    "knowledge",
    "cloudkit",
    "addressbook",
    "crashreporter",
    "diagnosticreports",
    "groupcontainers",
    "containers",
    "preferences",
    "caches",
    // Ours
    "freesize",
    "disktree",
];

/// Prefixes of ids that are the system's, never a leftover.
const SYSTEM_IDS: &[&str] = &["com.apple.", "group.com.apple."];

/// A folder's name as a key: lower case, letters and digits only, without
/// versions, architectures and anything in brackets. "Mozilla Firefox
/// (x64 en-US)" and "mozilla-firefox" both become `mozillafirefox`.
pub fn key(name: &str) -> String {
    let mut text = name.to_lowercase();
    for suffix in [".savedstate", ".plist"] {
        if let Some(stem) = text.strip_suffix(suffix) {
            text = stem.to_string();
        }
    }
    if let Some(open) = text.find('(') {
        text.truncate(open);
    }
    text.split_whitespace()
        .filter(|word| {
            let version = word
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | 'v'))
                && word.chars().any(|c| c.is_ascii_digit());
            !version
                && !matches!(
                    *word,
                    "x64" | "x86" | "64-bit" | "32-bit" | "amd64" | "arm64" | "win64" | "win32"
                )
        })
        .flat_map(|word| word.chars().filter(char::is_ascii_alphanumeric))
        .collect()
}

/// Keys too short to mean one program.
fn usable(key: &str) -> bool {
    key.len() >= 3 && !key.chars().all(|c| c.is_ascii_digit())
}

fn never(name: &str) -> bool {
    let lower = name.to_lowercase();
    SYSTEM_IDS.iter().any(|p| lower.starts_with(p)) || NEVER.contains(&key(name).as_str())
}

/// The installed programs by the keys their folders go by.
struct Index<'a> {
    by_key: HashMap<String, &'a Program>,
    ids: HashSet<&'a str>,
    /// Lower-case install locations.
    locations: HashMap<String, &'a Program>,
    /// The first two parts of the installed bundle ids (`com.docker`), so
    /// an installed app's helpers aren't called leftovers.
    bundle_vendors: HashSet<String>,
}

impl<'a> Index<'a> {
    fn build(programs: &'a [Program]) -> Index<'a> {
        let mut index = Index {
            by_key: HashMap::new(),
            ids: HashSet::new(),
            locations: HashMap::new(),
            bundle_vendors: HashSet::new(),
        };
        for program in programs {
            index.ids.insert(&program.id);
            for k in program.keys() {
                index.by_key.entry(k).or_insert(program);
            }
            if let Some(location) = &program.location {
                index.locations.insert(location_key(location), program);
            }
            if let Some(id) = program.id.strip_prefix("app:")
                && let Some(vendor) = vendor_of(id)
            {
                index.bundle_vendors.insert(vendor.to_string());
            }
        }
        index
    }

    fn installed(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    /// The program a folder called `name` belongs to, under a vendor's
    /// folder or not. Under a vendor, the program's name must carry the
    /// vendor ("Google Chrome" for `Google\Chrome`), or its publisher must.
    fn matching(&self, vendor: Option<&str>, name: &str) -> Option<&'a Program> {
        let name_key = key(name);
        if !usable(&name_key) {
            return None;
        }
        let Some(vendor) = vendor else {
            return self.by_key.get(&name_key).copied();
        };
        let vendor_key = key(vendor);
        if !usable(&vendor_key) {
            return None;
        }
        if let Some(program) = self.by_key.get(&format!("{vendor_key}{name_key}")) {
            return Some(program);
        }
        let program = *self.by_key.get(&name_key)?;
        let publisher = key(&program.vendor);
        (usable(&publisher)
            && (publisher.starts_with(&vendor_key) || vendor_key.starts_with(&publisher)))
        .then_some(program)
    }
}

/// The vendor part of a bundle id: `com.docker` of `com.docker.helper`.
fn vendor_of(id: &str) -> Option<&str> {
    let mut dots = id.match_indices('.');
    dots.next()?;
    let (second, _) = dots.next()?;
    Some(&id[..second])
}

fn location_key(path: &Path) -> String {
    path.to_string_lossy()
        .trim_end_matches(['/', '\\'])
        .to_lowercase()
}

/// An entry of a container that may be a leftover.
struct Candidate {
    path: PathBuf,
    note: String,
}

/// The leftovers among the platform's places, given what's installed. The
/// record is brought up to date: present programs and the folders matched
/// to them are noted, folders that are gone are forgotten.
pub fn find(
    platform: Platform,
    env: &Env,
    installed: &Installed,
    record: &mut Record,
    now: i64,
) -> Vec<Item> {
    if installed.programs.len() < BELIEVABLE {
        return Vec::new();
    }
    let index = Index::build(&installed.programs);
    for program in &installed.programs {
        let bare = program.id.split_once(':').map_or("", |(_, rest)| rest);
        if never(bare) {
            continue;
        }
        record.programs.insert(
            program.id.clone(),
            Seen {
                name: program.name.clone(),
                seen: now,
            },
        );
    }

    let mut candidates = Vec::new();
    for container in containers(platform, env) {
        for (name, path) in entries(&container.dir) {
            // The system's own folders are never offered, but on Windows
            // they hold programs' folders (`Microsoft\Edge`).
            let skip = never(&name);
            if skip && !container.two_levels {
                continue;
            }
            if let Some(strong) = container.strong
                && !skip
            {
                let id = strong.id(&name);
                if index.installed(&id) {
                    record.folders.insert(path_key(&path), id);
                    continue;
                }
                if installed.complete.contains(&strong) && strong_leftover(strong, &name, &index) {
                    candidates.push(Candidate {
                        path,
                        note: format!("{}: no such {} any more", trim_id(&name), strong.what()),
                    });
                    continue;
                }
            }
            if !skip
                && let Some(program) = index
                    .locations
                    .get(&location_key(&path))
                    .copied()
                    .or_else(|| index.matching(None, &name))
            {
                record.folders.insert(path_key(&path), program.id.clone());
                continue;
            }
            if container.two_levels && path.is_dir() {
                for (child, child_path) in entries(&path) {
                    if never(&child) {
                        continue;
                    }
                    if let Some(program) = index
                        .locations
                        .get(&location_key(&child_path))
                        .copied()
                        .or_else(|| index.matching(Some(&name), &child))
                    {
                        record
                            .folders
                            .insert(path_key(&child_path), program.id.clone());
                    } else if let Some(candidate) = recorded(record, &index, &child_path) {
                        candidates.push(candidate);
                    }
                }
            }
            if !skip && let Some(candidate) = recorded(record, &index, &path) {
                candidates.push(candidate);
            }
        }
    }

    // Folders that are gone are forgotten, as are programs not seen for
    // long with no folder of theirs left.
    record.folders.retain(|path, _| Path::new(path).exists());
    let kept: HashSet<&String> = record.folders.values().collect();
    record
        .programs
        .retain(|id, seen| kept.contains(id) || now - seen.seen < FORGET_AFTER);

    let cutoff = now - i64::from(AGE_DAYS) * 86_400;
    let mut offered = HashSet::new();
    let mut items: Vec<Item> = candidates
        .into_iter()
        .filter_map(|candidate| {
            if !offered.insert(candidate.path.clone()) {
                return None;
            }
            let meta = fs::symlink_metadata(&candidate.path).ok()?;
            let (bytes, files, newest) = if meta.is_dir() {
                crate::junk::size_and_newest(&candidate.path)
            } else {
                (meta.len(), 1, crate::junk::modified(&meta))
            };
            (bytes > 0 && newest <= cutoff).then_some(Item {
                path: candidate.path,
                bytes,
                files,
                covers: Covers::Itself,
                note: candidate.note,
            })
        })
        .collect();
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    items
}

/// A recorded folder whose program is gone.
fn recorded(record: &Record, index: &Index, path: &Path) -> Option<Candidate> {
    let id = record.folders.get(&path_key(path))?;
    if index.installed(id) {
        return None;
    }
    let seen = record.programs.get(id)?;
    Some(Candidate {
        path: path.to_path_buf(),
        note: format!("{}, installed until {}", seen.name, date(seen.seen)),
    })
}

/// Whether an id-named entry with no program of that id is a leftover:
/// the name must look like an id, and not be one of an installed app's
/// helpers.
fn strong_leftover(strong: Strong, name: &str, index: &Index) -> bool {
    let id = trim_id(name);
    match strong {
        Strong::Flatpak => id.matches('.').count() >= 2,
        Strong::Snap => !id.starts_with('.'),
        Strong::Bundle => {
            id.matches('.').count() >= 2
                && !vendor_of(&id.to_lowercase())
                    .is_some_and(|vendor| index.bundle_vendors.contains(vendor))
        }
    }
}

/// A bundle id from the names macOS gives its files:
/// `com.x.y.savedState`, `com.x.y.plist`.
fn trim_id(name: &str) -> &str {
    name.strip_suffix(".savedState")
        .or_else(|| name.strip_suffix(".plist"))
        .unwrap_or(name)
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// A folder's entries, sorted by name, with unreadable names skipped.
fn entries(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, PathBuf)> = read
        .flatten()
        .filter_map(|entry| Some((entry.file_name().to_str()?.to_string(), entry.path())))
        .collect();
    entries.sort();
    entries
}

/// `2026-10-03` for seconds since 1970.
pub fn date(secs: i64) -> String {
    // Howard Hinnant's civil-from-days.
    let days = secs.div_euclid(86_400) + 719_468;
    let era = days.div_euclid(146_097);
    let doe = days.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

// ----- What's installed, per platform --------------------------------------

/// The programs installed on this machine, read the platform's way. Empty
/// for a platform this isn't.
pub fn installed(platform: Platform) -> Installed {
    match platform {
        #[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
        Platform::Linux => linux::installed(),
        #[cfg(target_os = "macos")]
        Platform::MacOs => macos::installed(),
        #[cfg(windows)]
        Platform::Windows => windows::installed(),
        _ => Installed::default(),
    }
}

/// A `.desktop` file as a program: its stem (and the stem's last dotted
/// part), `Name`, `StartupWMClass` and the executable in `Exec` are the
/// names its folders go by.
pub fn desktop_entry(file: &Path, text: &str) -> Option<Program> {
    let stem = file.file_stem()?.to_str()?.to_string();
    let mut in_entry = false;
    let mut program = Program::new(&format!("desktop:{}", stem.to_lowercase()), "");
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        if let Some(name) = line.strip_prefix("Name=") {
            program.name = name.trim().to_string();
        } else if let Some(class) = line.strip_prefix("StartupWMClass=") {
            program.names.push(class.trim().to_string());
        } else if let Some(exec) = line.strip_prefix("Exec=")
            && let Some(word) = exec.split_whitespace().find(|w| !w.contains('='))
            && let Some(file) = Path::new(word.trim_matches('"')).file_name()
        {
            program.names.push(file.to_string_lossy().into_owned());
        }
    }
    if program.name.is_empty() {
        return None;
    }
    program.names.push(stem.clone());
    if let Some((_, short)) = stem.rsplit_once('.') {
        program.names.push(short.to_string());
    }
    Some(program)
}

/// The installed packages in dpkg's status file.
pub fn dpkg_status(text: &str) -> Vec<Program> {
    let mut programs = Vec::new();
    for stanza in text.split("\n\n") {
        let mut name = None;
        let mut installed = false;
        for line in stanza.lines() {
            if let Some(n) = line.strip_prefix("Package: ") {
                name = Some(n.trim());
            } else if let Some(status) = line.strip_prefix("Status: ") {
                installed = status.trim().ends_with(" installed");
            }
        }
        if let Some(name) = name
            && installed
        {
            programs.push(Program::new(&format!("deb:{name}"), name));
        }
    }
    programs
}

/// The package of a folder in pacman's database: `htop-3.3.0-1` is `htop`.
pub fn pacman_package(folder: &str) -> Option<&str> {
    let (rest, _release) = folder.rsplit_once('-')?;
    let (name, _version) = rest.rsplit_once('-')?;
    (!name.is_empty()).then_some(name)
}

/// The programs in `reg query … /s` output of an Uninstall key: each
/// subkey with a DisplayName that isn't a system component or a patch.
pub fn reg_query(text: &str) -> Vec<Program> {
    let mut programs = Vec::new();
    let mut current: Option<(String, Program, bool)> = None;
    let finish = |current: &mut Option<(String, Program, bool)>, programs: &mut Vec<Program>| {
        if let Some((_, program, skip)) = current.take()
            && !skip
            && !program.name.is_empty()
            && !program.name.contains("(KB")
        {
            programs.push(program);
        }
    };
    for line in text.lines() {
        if line.starts_with("HKEY_") {
            finish(&mut current, &mut programs);
            let name = line.trim().rsplit('\\').next().unwrap_or("").to_string();
            current = Some((
                name.clone(),
                Program::new(&format!("win:{name}"), ""),
                false,
            ));
            continue;
        }
        let Some((_, program, skip)) = current.as_mut() else {
            continue;
        };
        let mut parts = line.trim().splitn(3, "    ");
        let (Some(value_name), Some(_kind), value) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let value = value.unwrap_or("").trim();
        match value_name {
            "DisplayName" => program.name = value.to_string(),
            "Publisher" => program.vendor = value.to_string(),
            "InstallLocation" if !value.is_empty() => {
                program.location = Some(PathBuf::from(value.trim_matches('"')));
            }
            "SystemComponent" if value == "0x1" => *skip = true,
            "ParentKeyName" => *skip = true,
            _ => {}
        }
    }
    finish(&mut current, &mut programs);
    programs
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "android")))]
mod linux {
    use super::*;

    pub fn installed() -> Installed {
        let home = std::env::home_dir().unwrap_or_default();
        let mut installed = Installed::default();
        for dir in data_dirs(&home) {
            desktop_files(&dir.join("applications"), 0, &mut installed.programs);
        }
        for dir in [
            PathBuf::from("/var/lib/flatpak/app"),
            home.join(".local/share/flatpak/app"),
        ] {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            if !installed.complete.contains(&Strong::Flatpak) {
                installed.complete.push(Strong::Flatpak);
            }
            for entry in entries.flatten() {
                let id = entry.file_name().to_string_lossy().into_owned();
                let name = id.rsplit('.').next().unwrap_or(&id).to_string();
                let mut program = Program::new(&format!("flatpak:{id}"), &name);
                program.names.push(id.clone());
                installed.programs.push(program);
            }
        }
        for dir in ["/snap", "/var/lib/snapd/snap"] {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            let mut any = false;
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name == "bin" || name == "README" {
                    continue;
                }
                any = true;
                installed
                    .programs
                    .push(Program::new(&format!("snap:{name}"), &name));
            }
            if any && !installed.complete.contains(&Strong::Snap) {
                installed.complete.push(Strong::Snap);
            }
        }
        if let Ok(text) = fs::read_to_string("/var/lib/dpkg/status") {
            installed.programs.extend(dpkg_status(&text));
        }
        if let Ok(entries) = fs::read_dir("/var/lib/pacman/local") {
            for entry in entries.flatten() {
                let folder = entry.file_name().to_string_lossy().into_owned();
                if let Some(name) = pacman_package(&folder) {
                    installed
                        .programs
                        .push(Program::new(&format!("pacman:{name}"), name));
                }
            }
        }
        if Path::new("/usr/bin/rpm").exists()
            && let Ok(output) = std::process::Command::new("/usr/bin/rpm")
                .args(["-qa", "--qf", "%{NAME}\n"])
                .output()
            && output.status.success()
        {
            for name in String::from_utf8_lossy(&output.stdout).lines() {
                let name = name.trim();
                if !name.is_empty() {
                    installed
                        .programs
                        .push(Program::new(&format!("rpm:{name}"), name));
                }
            }
        }
        installed
    }

    /// `$XDG_DATA_HOME` and `$XDG_DATA_DIRS`, plus where Flatpak and Snap
    /// export their apps.
    fn data_dirs(home: &Path) -> Vec<PathBuf> {
        let mut dirs = vec![
            std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .filter(|d| d.is_absolute())
                .unwrap_or_else(|| home.join(".local/share")),
        ];
        let system = std::env::var("XDG_DATA_DIRS")
            .ok()
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
        dirs.extend(system.split(':').map(PathBuf::from));
        dirs.push(home.join(".local/share/flatpak/exports/share"));
        dirs.push(PathBuf::from("/var/lib/flatpak/exports/share"));
        dirs.push(PathBuf::from("/var/lib/snapd/desktop"));
        dirs
    }

    fn desktop_files(dir: &Path, depth: u32, out: &mut Vec<Program>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && depth < 2 {
                desktop_files(&path, depth + 1, out);
            } else if path.extension().is_some_and(|e| e == "desktop")
                && let Ok(text) = fs::read_to_string(&path)
                && let Some(program) = desktop_entry(&path, &text)
            {
                out.push(program);
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    /// Every app bundle Spotlight knows, plus the usual folders; the
    /// bundle ids come from each `Info.plist`.
    pub fn installed() -> Installed {
        let mut installed = Installed::default();
        let mut bundles: Vec<PathBuf> = Vec::new();
        if let Ok(output) = std::process::Command::new("mdfind")
            .arg("kMDItemContentType == 'com.apple.application-bundle'")
            .output()
            && output.status.success()
        {
            bundles.extend(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(PathBuf::from),
            );
        }
        if bundles.len() >= 10 {
            installed.complete.push(Strong::Bundle);
        }
        let mut dirs = vec![
            PathBuf::from("/Applications"),
            PathBuf::from("/Applications/Utilities"),
            PathBuf::from("/System/Applications"),
            PathBuf::from("/System/Applications/Utilities"),
        ];
        if let Some(home) = std::env::home_dir() {
            dirs.push(home.join("Applications"));
        }
        for dir in dirs {
            let Ok(entries) = fs::read_dir(dir) else {
                continue;
            };
            bundles.extend(
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "app")),
            );
        }
        bundles.sort();
        bundles.dedup();
        for bundle in bundles {
            if let Some(program) = read_bundle(&bundle) {
                installed.programs.push(program);
            }
        }
        installed
    }

    fn read_bundle(bundle: &Path) -> Option<Program> {
        let stem = bundle.file_stem()?.to_str()?.to_string();
        let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).ok()?;
        let dict = info.as_dictionary()?;
        let text = |key: &str| {
            dict.get(key)
                .and_then(|v| v.as_string())
                .map(str::to_string)
        };
        let id = text("CFBundleIdentifier")?;
        let mut program = Program::new(
            &format!("app:{}", id.to_lowercase()),
            &text("CFBundleDisplayName")
                .or_else(|| text("CFBundleName"))
                .unwrap_or_else(|| stem.clone()),
        );
        program.names.push(stem);
        program.names.extend(text("CFBundleName"));
        program.location = Some(bundle.to_path_buf());
        Some(program)
    }
}

#[cfg(windows)]
mod windows {
    use std::os::windows::process::CommandExt;

    use super::*;

    /// The Uninstall keys the Settings app's list comes from.
    pub fn installed() -> Installed {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let mut installed = Installed::default();
        for key in [
            r"HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            r"HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
            r"HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ] {
            let Ok(output) = std::process::Command::new("reg.exe")
                .args(["query", key, "/s"])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            else {
                continue;
            };
            if output.status.success() {
                installed
                    .programs
                    .extend(reg_query(&String::from_utf8_lossy(&output.stdout)));
            }
        }
        installed
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::time::{Duration, SystemTime};

    use super::*;

    /// A file `age` days old (a folder when the path ends with `/`).
    fn put(env: &Env, template: &str, age: u64) {
        let path = env.expand(template);
        if template.ends_with('/') {
            fs::create_dir_all(&path).unwrap();
            return;
        }
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"left behind").unwrap();
        let then = SystemTime::now() - Duration::from_secs(age * 86_400);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(then)
            .unwrap();
    }

    fn now() -> i64 {
        crate::junk::now()
    }

    /// Enough made-up programs to be believed.
    fn fillers() -> Vec<Program> {
        (0..10)
            .map(|i| Program::new(&format!("deb:filler{i}"), &format!("filler{i}")))
            .collect()
    }

    fn paths(items: &[Item], env: &Env) -> Vec<String> {
        items
            .iter()
            .map(|i| {
                i.path
                    .strip_prefix(&env.home)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn keys_drop_versions_and_brackets() {
        assert_eq!(key("Mozilla Firefox (x64 en-US)"), "mozillafirefox");
        assert_eq!(key("mozilla-firefox"), "mozillafirefox");
        assert_eq!(key("7-Zip 23.01 (x64)"), "7zip");
        assert_eq!(key("Python 3.12.1 (64-bit)"), "python");
        assert_eq!(key("Visual Studio Code (User)"), "visualstudiocode");
        assert_eq!(key("com.spotify.client.savedState"), "comspotifyclient");
        assert_eq!(key("Code"), "code");
        assert!(!usable(&key("gh")));
        assert!(!usable("123"));
        assert_eq!(vendor_of("com.docker.helper"), Some("com.docker"));
        assert_eq!(vendor_of("docker"), None);
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_791_000_000), "2026-10-03");
    }

    #[test]
    fn linux_leftovers_need_the_record_or_an_id() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::under(dir.path());
        put(&env, "~/.config/Code/User/settings.json", 100);
        put(&env, "~/.config/htop/htoprc", 100);
        put(&env, "~/.config/unknown-thing/x", 100);
        put(&env, "~/.config/dconf/user", 100);
        put(&env, "~/.var/app/org.gnome.Maps/data/x", 100);
        put(&env, "~/.var/app/org.gnome.Gone/data/x", 100);
        put(&env, "~/.var/app/org.gnome.Fresh/data/x", 3);
        put(&env, "~/.var/app/org.gnome.Empty/", 0);
        put(&env, "~/snap/gone-snap/common/x", 100);
        put(&env, "~/snap/spotify/common/x", 100);

        let mut code = Program::new("desktop:code", "Visual Studio Code");
        code.names.push("code".into());
        let programs = |with_code: bool| {
            let mut programs = fillers();
            if with_code {
                programs.push(code.clone());
                programs.push(Program::new("deb:htop", "htop"));
                programs.push(Program::new("deb:dconf", "dconf"));
            }
            programs.push(Program::new("flatpak:org.gnome.Maps", "Maps"));
            programs.push(Program::new("snap:spotify", "spotify"));
            programs
        };
        let mut record = Record::default();

        // First look: everything installed; only the id-named strangers go.
        let installed = Installed {
            programs: programs(true),
            complete: vec![Strong::Flatpak, Strong::Snap],
        };
        let items = find(Platform::Linux, &env, &installed, &mut record, now());
        assert_eq!(
            paths(&items, &env),
            [".var/app/org.gnome.Gone", "snap/gone-snap"]
        );
        assert_eq!(items[0].note, "org.gnome.Gone: no such Flatpak any more");
        assert_eq!(items[0].bytes, 11);
        assert_eq!(
            record.folders.get(&path_key(&env.expand("~/.config/Code"))),
            Some(&"desktop:code".to_string())
        );
        assert_eq!(
            record.folders.get(&path_key(&env.expand("~/.config/htop"))),
            Some(&"deb:htop".to_string())
        );
        // dconf's folder is never recorded, nor unknown-thing's.
        assert!(
            !record
                .folders
                .contains_key(&path_key(&env.expand("~/.config/dconf")))
        );
        assert!(
            !record
                .folders
                .contains_key(&path_key(&env.expand("~/.config/unknown-thing")))
        );
        assert!(record.programs.contains_key("desktop:code"));

        // Without the snap list read, snaps aren't judged.
        let partial = Installed {
            programs: programs(true),
            complete: vec![Strong::Flatpak],
        };
        let items = find(Platform::Linux, &env, &partial, &mut record.clone(), now());
        assert_eq!(paths(&items, &env), [".var/app/org.gnome.Gone"]);

        // Second look, Code and htop uninstalled: their folders are
        // leftovers, named after them.
        let later = now() + 86_400;
        let gone = Installed {
            programs: programs(false),
            complete: vec![Strong::Flatpak, Strong::Snap],
        };
        let items = find(Platform::Linux, &env, &gone, &mut record, later);
        assert_eq!(
            paths(&items, &env),
            [
                ".config/Code",
                ".config/htop",
                ".var/app/org.gnome.Gone",
                "snap/gone-snap"
            ]
        );
        let code_item = items.iter().find(|i| i.path.ends_with("Code")).unwrap();
        assert_eq!(
            code_item.note,
            format!("Visual Studio Code, installed until {}", date(now()))
        );
        assert_eq!(code_item.covers, Covers::Itself);

        // Code written to again recently: not offered.
        put(&env, "~/.config/Code/User/state.json", 1);
        let items = find(Platform::Linux, &env, &gone, &mut record, later);
        assert!(!paths(&items, &env).contains(&".config/Code".to_string()));

        // A record is pruned when a folder goes.
        fs::remove_dir_all(env.expand("~/.config/htop")).unwrap();
        find(Platform::Linux, &env, &gone, &mut record, later);
        assert!(
            !record
                .folders
                .contains_key(&path_key(&env.expand("~/.config/htop")))
        );

        // Too few programs read: nothing is offered, nothing recorded.
        let before = record.clone();
        let few = Installed {
            programs: vec![Program::new("deb:one", "one")],
            complete: vec![Strong::Flatpak, Strong::Snap],
        };
        assert!(find(Platform::Linux, &env, &few, &mut record, later).is_empty());
        assert_eq!(record, before);
    }

    #[test]
    fn windows_vendor_folders_match_by_publisher_and_location() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::under(dir.path());
        put(&env, "$LOCAL/Google/Chrome/User Data/Default/Cookies", 100);
        put(&env, "$LOCAL/Google/Update/x.log", 100);
        put(&env, "$LOCAL/Discord/app-1.0/x.dll", 100);
        put(&env, "$ROAMING/discord/Local Storage/x", 100);
        put(&env, "$LOCAL/Programs/Foo Tool/foo.exe", 100);
        put(&env, "$LOCAL/Valve/Steam/x", 100);
        put(&env, "$LOCAL/Microsoft/Edge/User Data/x", 100);
        put(&env, "$LOCAL/Temp/x.tmp", 100);

        let programs = |with: &[&str]| {
            let mut programs = fillers();
            let mut chrome = Program::new("win:Google Chrome", "Google Chrome");
            chrome.vendor = "Google LLC".into();
            let mut discord = Program::new("win:Discord", "Discord");
            discord.vendor = "Discord Inc.".into();
            let mut foo = Program::new("win:{1234}", "Foo Tool 2.1");
            foo.location = Some(env.expand("$LOCAL/Programs/Foo Tool/"));
            let mut steam = Program::new("win:Steam", "Steam");
            steam.vendor = "Valve Corporation".into();
            let mut edge = Program::new("win:Microsoft Edge", "Microsoft Edge");
            edge.vendor = "Microsoft Corporation".into();
            for program in [chrome, discord, foo, steam, edge] {
                if with.contains(&program.name.as_str()) {
                    programs.push(program);
                }
            }
            programs
        };
        let mut record = Record::default();
        let all = Installed {
            programs: programs(&[
                "Google Chrome",
                "Discord",
                "Foo Tool 2.1",
                "Steam",
                "Microsoft Edge",
            ]),
            complete: Vec::new(),
        };
        assert!(find(Platform::Windows, &env, &all, &mut record, now()).is_empty());
        let recorded = |record: &Record, template: &str| {
            record
                .folders
                .get(&path_key(&env.expand(template)))
                .cloned()
        };
        assert_eq!(
            recorded(&record, "$LOCAL/Google/Chrome").as_deref(),
            Some("win:Google Chrome")
        );
        assert_eq!(recorded(&record, "$LOCAL/Google"), None);
        assert_eq!(recorded(&record, "$LOCAL/Google/Update"), None);
        assert_eq!(
            recorded(&record, "$LOCAL/Discord").as_deref(),
            Some("win:Discord")
        );
        assert_eq!(
            recorded(&record, "$ROAMING/discord").as_deref(),
            Some("win:Discord")
        );
        assert_eq!(
            recorded(&record, "$LOCAL/Programs/Foo Tool").as_deref(),
            Some("win:{1234}")
        );
        assert_eq!(
            recorded(&record, "$LOCAL/Valve/Steam").as_deref(),
            Some("win:Steam")
        );
        assert_eq!(
            recorded(&record, "$LOCAL/Microsoft/Edge").as_deref(),
            Some("win:Microsoft Edge")
        );
        assert_eq!(recorded(&record, "$LOCAL/Temp"), None);

        let fewer = Installed {
            programs: programs(&["Steam", "Microsoft Edge"]),
            complete: Vec::new(),
        };
        let items = find(Platform::Windows, &env, &fewer, &mut record, now() + 60);
        let found: Vec<String> = items
            .iter()
            .map(|i| {
                i.path
                    .strip_prefix(dir.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            found,
            [
                "home/AppData/Local/Discord",
                "home/AppData/Local/Google/Chrome",
                "home/AppData/Local/Programs/Foo Tool",
                "home/AppData/Roaming/discord",
            ]
        );
        assert!(items[0].note.starts_with("Discord, installed until "));
        assert!(items[2].note.starts_with("Foo Tool 2.1, installed until "));
    }

    #[test]
    fn macos_bundle_ids_spare_apples_and_helpers() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::under(dir.path());
        put(&env, "~/Library/Containers/com.gone.app/Data/x", 100);
        put(&env, "~/Library/Containers/com.apple.mail/Data/x", 100);
        put(&env, "~/Library/Containers/com.docker.helper/Data/x", 100);
        put(&env, "~/Library/Containers/com.docker.docker/Data/x", 100);
        put(&env, "~/Library/Preferences/com.gone.app.plist", 100);
        put(&env, "~/Library/Preferences/com.docker.docker.plist", 100);
        put(
            &env,
            "~/Library/Saved Application State/com.gone.app.savedState/x",
            100,
        );
        put(&env, "~/Library/Application Support/Sketch/x", 100);
        put(
            &env,
            "~/Library/Application Support/MobileSync/Backup/x",
            100,
        );
        put(&env, "~/Library/Caches/com.gone.app/x", 100);

        let programs = |with_sketch: bool| {
            let mut programs = fillers();
            let mut docker = Program::new("app:com.docker.docker", "Docker");
            docker.names.push("Docker".into());
            programs.push(docker);
            programs.push(Program::new("app:com.apple.mail", "Mail"));
            if with_sketch {
                let mut sketch = Program::new("app:com.bohemiancoding.sketch3", "Sketch");
                sketch.names.push("Sketch".into());
                programs.push(sketch);
            }
            programs
        };
        let mut record = Record::default();
        let complete = Installed {
            programs: programs(true),
            complete: vec![Strong::Bundle],
        };
        let items = find(Platform::MacOs, &env, &complete, &mut record, now());
        assert_eq!(
            paths(&items, &env),
            [
                "Library/Caches/com.gone.app",
                "Library/Containers/com.gone.app",
                "Library/Preferences/com.gone.app.plist",
                "Library/Saved Application State/com.gone.app.savedState",
            ]
        );
        assert_eq!(items[2].note, "com.gone.app: no such app any more");
        assert_eq!(items[2].files, 1);
        assert!(!record.programs.contains_key("app:com.apple.mail"));

        // Spotlight didn't answer: only the record counts.
        let partial = Installed {
            programs: programs(false),
            complete: Vec::new(),
        };
        let items = find(Platform::MacOs, &env, &partial, &mut record, now() + 60);
        assert_eq!(paths(&items, &env), ["Library/Application Support/Sketch"]);
        assert!(items[0].note.starts_with("Sketch, installed until "));
    }

    #[test]
    fn parsers_read_desktop_dpkg_pacman_and_the_registry() {
        let code = desktop_entry(
            Path::new("/usr/share/applications/code.desktop"),
            "[Desktop Entry]\nName=Visual Studio Code\nExec=/usr/share/code/code --unity-launch %F\nStartupWMClass=Code\n[Desktop Action new]\nName=New Window\n",
        )
        .unwrap();
        assert_eq!(code.id, "desktop:code");
        assert_eq!(code.name, "Visual Studio Code");
        assert_eq!(code.names, ["code", "Code", "code"]);
        let firefox = desktop_entry(
            Path::new("/x/org.mozilla.firefox.desktop"),
            "[Desktop Entry]\nName=Firefox\nExec=firefox %u\n",
        )
        .unwrap();
        assert_eq!(firefox.names, ["firefox", "org.mozilla.firefox", "firefox"]);
        assert!(
            desktop_entry(
                Path::new("/x/empty.desktop"),
                "[Desktop Entry]\nType=Link\n"
            )
            .is_none()
        );

        let dpkg = dpkg_status(
            "Package: htop\nStatus: install ok installed\nVersion: 3.3.0\n\nPackage: gone\nStatus: deinstall ok config-files\n\nPackage: vim\nStatus: install ok installed\n",
        );
        let ids: Vec<&str> = dpkg.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["deb:htop", "deb:vim"]);

        assert_eq!(pacman_package("htop-3.3.0-1"), Some("htop"));
        assert_eq!(
            pacman_package("python-requests-2.32.3-1"),
            Some("python-requests")
        );
        assert_eq!(pacman_package("odd"), None);

        let reg = reg_query(
            "\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\7-Zip\r\n    DisplayName    REG_SZ    7-Zip 23.01 (x64)\r\n    DisplayVersion    REG_SZ    23.01\r\n    Publisher    REG_SZ    Igor Pavlov\r\n    InstallLocation    REG_SZ    C:\\Program Files\\7-Zip\\\r\n\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{AC76BA86-1033-FFFF-7760-BC15014EA700}\r\n    DisplayName    REG_SZ    Adobe Acrobat (64-bit)\r\n    Publisher    REG_SZ    Adobe\r\n    SystemComponent    REG_DWORD    0x0\r\n\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{hidden}\r\n    DisplayName    REG_SZ    Hidden Runtime\r\n    SystemComponent    REG_DWORD    0x1\r\n\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{patch}\r\n    DisplayName    REG_SZ    Update for Thing (KB123)\r\n    ParentKeyName    REG_SZ    Thing\r\n\r\nHKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\NoName\r\n    UninstallString    REG_SZ    x.exe\r\n",
        );
        assert_eq!(reg.len(), 2);
        assert_eq!(reg[0].id, "win:7-Zip");
        assert_eq!(reg[0].name, "7-Zip 23.01 (x64)");
        assert_eq!(reg[0].vendor, "Igor Pavlov");
        assert_eq!(
            reg[0].location.as_deref(),
            Some(Path::new("C:\\Program Files\\7-Zip\\"))
        );
        assert_eq!(reg[1].id, "win:{AC76BA86-1033-FFFF-7760-BC15014EA700}");
        assert_eq!(reg[1].name, "Adobe Acrobat (64-bit)");
    }

    #[test]
    fn record_round_trips_and_finding_sums() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/programs.json");
        let mut record = Record::default();
        record.programs.insert(
            "deb:htop".into(),
            Seen {
                name: "htop".into(),
                seen: 5,
            },
        );
        record
            .folders
            .insert("/home/x/.config/htop".into(), "deb:htop".into());
        record.save(&path).unwrap();
        assert_eq!(Record::load(&path), record);
        assert_eq!(
            Record::load(&dir.path().join("none.json")),
            Record::default()
        );

        assert!(finding(Vec::new()).is_none());
        let found = finding(vec![
            Item {
                path: PathBuf::from("/a"),
                bytes: 5,
                files: 2,
                covers: Covers::Itself,
                note: "A".into(),
            },
            Item {
                path: PathBuf::from("/b"),
                bytes: 7,
                files: 1,
                covers: Covers::Itself,
                note: "B".into(),
            },
        ])
        .unwrap();
        assert_eq!(found.rule.id, ID);
        assert_eq!((found.bytes, found.files), (12, 3));
        assert!(!found.ticked());
        assert_eq!(found.why_not(), "Look first; goes to the quarantine.");
        assert!(look(Platform::Android, &Env::under(dir.path()), 0, None).is_none());
    }
}
