//! Space on a drive that no file accounts for: ZFS, btrfs and APFS
//! snapshots, shadow copies, purgeable space, the file system itself, files
//! that couldn't be read.
//!
//! Only meaningful when the whole drive was scanned. The probes ask the
//! system's own tools and say nothing when a tool is missing or needs more
//! rights.

use std::path::Path;
use std::process::Command;

use freesize_core::format_size;

/// Whether `path` is the top of its file system, so a scan of it covers the
/// whole drive.
pub fn is_drive_root(path: &Path) -> bool {
    if cfg!(target_os = "android") {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Some(parent) = path.parent() else {
            return true;
        };
        match (std::fs::metadata(path), std::fs::metadata(parent)) {
            (Ok(own), Ok(up)) => own.dev() != up.dev(),
            _ => false,
        }
    }
    #[cfg(windows)]
    {
        // "C:\" has a prefix and a root and nothing else.
        path.components().count() == 2 && path.has_root()
    }
}

/// What the system says about space outside the files, as sentences.
pub fn probe(root: &Path) -> Vec<String> {
    let mut notes = Vec::new();
    if cfg!(target_os = "linux")
        && let Some(out) = run(
            "zfs",
            &["list", "-H", "-p", "-o", "name,usedbysnapshots,mountpoint"],
        )
        && let Some((dataset, bytes)) = parse_zfs(&out, &root.to_string_lossy())
        && bytes > 0
    {
        notes.push(format!(
            "ZFS snapshots of {dataset} hold {}. Deleting files they still contain frees \
             nothing until those snapshots are destroyed.",
            format_size(bytes)
        ));
    }
    if cfg!(target_os = "linux")
        && let Some(out) = run(
            "btrfs",
            &["subvolume", "list", "-s", &root.to_string_lossy()],
        )
    {
        let ids = parse_btrfs_snapshots(&out);
        if !ids.is_empty() {
            let exclusive = run(
                "btrfs",
                &["qgroup", "show", "--raw", &root.to_string_lossy()],
            )
            .map(|q| parse_btrfs_qgroups(&q, &ids));
            notes.push(match exclusive {
                Some(bytes) if bytes > 0 => format!(
                    "{} btrfs snapshot{} hold {} that nothing else uses. Deleting files they \
                     still contain frees nothing until those snapshots are deleted (btrfs \
                     subvolume delete, or the tool that makes them).",
                    ids.len(),
                    if ids.len() == 1 { "" } else { "s" },
                    format_size(bytes)
                ),
                _ => format!(
                    "{} btrfs snapshot{} hold some of it; with quotas on (btrfs quota enable) \
                     FreeSize can say how much.",
                    ids.len(),
                    if ids.len() == 1 { "" } else { "s" }
                ),
            });
        }
    }
    if cfg!(windows) {
        let drive = root.to_string_lossy().trim_end_matches('\\').to_string();
        match run(
            "vssadmin",
            &["list", "shadowstorage", &format!("/for={drive}")],
        )
        .and_then(|out| parse_vssadmin(&out))
        {
            Some(bytes) if bytes > 0 => notes.push(format!(
                "System Restore points and shadow copies use {}. System Properties → System \
                 Protection → Configure can limit or delete them.",
                format_size(bytes)
            )),
            Some(_) => {}
            None => notes.push(
                "System Restore points may use some of it; System Properties → System \
                 Protection → Configure shows how much."
                    .into(),
            ),
        }
    }
    if cfg!(target_os = "macos") {
        let time_machine = run("tmutil", &["listlocalsnapshots", &root.to_string_lossy()])
            .map_or(0, |out| parse_tmutil(&out));
        if time_machine > 0 {
            notes.push(format!(
                "{time_machine} local Time Machine snapshot{} hold some of it; macOS removes \
                 them as space runs low.",
                if time_machine == 1 { "" } else { "s" }
            ));
        }
        let others = run(
            "diskutil",
            &["apfs", "listSnapshots", &root.to_string_lossy()],
        )
        .map_or(0, |out| {
            parse_apfs_snapshots(&out).saturating_sub(time_machine)
        });
        if others > 0 {
            notes.push(format!(
                "{others} other APFS snapshot{} (a backup tool's, or the system's before an \
                 update) hold some of it until they're deleted.",
                if others == 1 { "" } else { "s" }
            ));
        }
        match run("diskutil", &["info", &root.to_string_lossy()])
            .and_then(|out| parse_purgeable(&out))
        {
            Some(bytes) if bytes > 0 => notes.push(format!(
                "{} is purgeable: caches, local snapshots and other things macOS drops by \
                 itself when space runs low. The Finder and About This Mac → Storage count it \
                 as available.",
                format_size(bytes)
            )),
            _ => notes.push(
                "macOS keeps purgeable space (caches, local snapshots, downloaded updates) \
                 that it drops when space runs low; About This Mac → Storage shows how much, \
                 as System Data."
                    .into(),
            ),
        }
    }
    notes
}

/// The subvolume ids `btrfs subvolume list -s` lists (snapshots only).
fn parse_btrfs_snapshots(output: &str) -> Vec<u64> {
    output
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            (words.next()? == "ID").then(|| words.next()?.parse().ok())?
        })
        .collect()
}

/// The exclusive bytes of the given subvolumes' qgroups (`0/<id>`), from
/// `btrfs qgroup show --raw`: "qgroupid  rfer  excl".
fn parse_btrfs_qgroups(output: &str, ids: &[u64]) -> u64 {
    output
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let id: u64 = words.next()?.strip_prefix("0/")?.parse().ok()?;
            let _referenced = words.next()?;
            let exclusive: u64 = words.next()?.parse().ok()?;
            ids.contains(&id).then_some(exclusive)
        })
        .sum()
}

/// How many snapshots `diskutil apfs listSnapshots` lists: one `Name:`
/// line each.
fn parse_apfs_snapshots(output: &str) -> usize {
    output.lines().filter(|l| l.contains("Name:")).count()
}

/// The purgeable bytes a `diskutil info` listing names, when it does.
fn parse_purgeable(output: &str) -> Option<u64> {
    let line = output.lines().find(|l| l.contains("Purgeable"))?;
    let (_, rest) = line.split_once('(')?;
    let number: u64 = rest.split_whitespace().next()?.parse().ok()?;
    Some(number)
}

fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The dataset mounted at `mountpoint` and the bytes its snapshots use, from
/// `zfs list -H -p -o name,usedbysnapshots,mountpoint`.
fn parse_zfs(output: &str, mountpoint: &str) -> Option<(String, u64)> {
    output.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let (name, used, mount) = (fields.next()?, fields.next()?, fields.next()?);
        (mount == mountpoint).then(|| (name.to_string(), used.trim().parse().unwrap_or(0)))
    })
}

/// "Used Shadow Copy Storage space: 4.26 GB (2%)" from `vssadmin list
/// shadowstorage`.
fn parse_vssadmin(output: &str) -> Option<u64> {
    let line = output
        .lines()
        .find(|l| l.contains("Used Shadow Copy Storage space"))?;
    let value = line.split(':').nth(1)?.trim();
    let mut parts = value.split_whitespace();
    let number: f64 = parts.next()?.replace(',', ".").parse().ok()?;
    let unit = parts.next()?;
    let scale: f64 = match unit {
        "B" | "bytes" => 1.0,
        "KB" => 1024.0,
        "MB" => 1024.0 * 1024.0,
        "GB" => 1024.0 * 1024.0 * 1024.0,
        "TB" => 1024.0_f64.powi(4),
        _ => return None,
    };
    Some((number * scale) as u64)
}

/// How many snapshots `tmutil listlocalsnapshots` lists.
fn parse_tmutil(output: &str) -> usize {
    output
        .lines()
        .filter(|l| l.contains("com.apple.TimeMachine"))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_output() {
        let zfs = "tank\t0\t/mnt/tank\ntank/media\t13207024435\t/mnt/tank/media\n";
        assert_eq!(
            parse_zfs(zfs, "/mnt/tank/media"),
            Some(("tank/media".to_string(), 13_207_024_435))
        );
        assert_eq!(parse_zfs(zfs, "/elsewhere"), None);

        let vss = "Shadow Copy Storage association\n   For volume: (C:)\\\\?\\Volume{x}\\\n   \
                   Used Shadow Copy Storage space: 4.26 GB (2%)\n   Allocated Shadow Copy \
                   Storage space: 4.6 GB (2%)\n";
        assert_eq!(
            parse_vssadmin(vss),
            Some((4.26 * 1024.0 * 1024.0 * 1024.0) as u64)
        );
        assert_eq!(parse_vssadmin("nothing here"), None);

        let tm = "Snapshots for disk /:\ncom.apple.TimeMachine.2026-10-11-101010.local\n\
                  com.apple.TimeMachine.2026-10-12-090909.local\n";
        assert_eq!(parse_tmutil(tm), 2);

        let subvolumes = "ID 257 gen 100 cgen 90 top level 5 otime 2026-10-01 10:00:00 path \
                          .snapshots/1/snapshot\nID 260 gen 120 cgen 110 top level 5 otime \
                          2026-10-08 10:00:00 path .snapshots/2/snapshot\n";
        let ids = parse_btrfs_snapshots(subvolumes);
        assert_eq!(ids, [257, 260]);
        let qgroups = "qgroupid   rfer        excl\n--------   ----        ----\n0/5        \
                       5000000000  1000000000\n0/257      4000000000  300000000\n0/260      \
                       4100000000  200000000\n";
        assert_eq!(parse_btrfs_qgroups(qgroups, &ids), 500_000_000);
        assert_eq!(
            parse_btrfs_qgroups("ERROR: can't list qgroups: quotas not enabled", &ids),
            0
        );

        let apfs = "Snapshots for disk1s1 (2 found)\n|\n+-- 1A2B\n|   Name:        \
                    com.apple.TimeMachine.2026-10-11-101010.local\n|   XID:         100\n|\n\
                    +-- 3C4D\n    Name:        com.bombich.ccc.safetynet\n    XID:         120\n";
        assert_eq!(parse_apfs_snapshots(apfs), 2);
        assert_eq!(parse_apfs_snapshots("No snapshots for disk1s1\n"), 0);

        let info = "   Volume Name:               Macintosh HD\n   Container Total Space:     \
                    494.4 GB (494384795648 Bytes)\n   Volume Free Space:         120.0 GB \
                    (120000000000 Bytes) (exactly 234375000 512-Byte-Units)\n   Purgeable \
                    Space:           30.0 GB (30000000000 Bytes)\n";
        assert_eq!(parse_purgeable(info), Some(30_000_000_000));
        assert_eq!(parse_purgeable("   Volume Name: X\n"), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_root_is_a_drive_root() {
        assert!(is_drive_root(Path::new("/")));
        assert!(!is_drive_root(Path::new("/usr/bin")) || cfg!(target_os = "android"));
    }
}
