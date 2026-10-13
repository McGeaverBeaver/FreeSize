//! Space on a drive that no file accounts for: snapshots, shadow copies,
//! the file system itself, files that couldn't be read.
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
    if cfg!(target_os = "macos")
        && let Some(out) = run("tmutil", &["listlocalsnapshots", &root.to_string_lossy()])
    {
        let count = parse_tmutil(&out);
        if count > 0 {
            notes.push(format!(
                "{count} local Time Machine snapshot{} hold some of it; macOS removes them as \
                 space runs low.",
                if count == 1 { "" } else { "s" }
            ));
        }
    }
    notes
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
    }

    #[cfg(unix)]
    #[test]
    fn the_root_is_a_drive_root() {
        assert!(is_drive_root(Path::new("/")));
        assert!(!is_drive_root(Path::new("/usr/bin")) || cfg!(target_os = "android"));
    }
}
