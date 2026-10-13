//! The junk cleaner on a device, for the Hub and the command line: the
//! core's findings as a report, and cleaning one rule's findings the way
//! its safety says: safe items are deleted, "check first" items go to the
//! quarantine, "use the tool" items are refused with the tool named, and a
//! rule whose program is running is skipped.

use std::path::PathBuf;

use freesize_core::junk::{self, Finding, Safety};
use freesize_proto::{JunkFinding, JunkReport};

/// The findings as the Hub stores them.
pub fn report(findings: &[Finding], now: i64) -> JunkReport {
    JunkReport {
        taken: now,
        total: junk::total(findings),
        safe: junk::safe_total(findings),
        entries: findings
            .iter()
            .map(|f| JunkFinding {
                kind: f.rule.id.into(),
                category: f.rule.category.id().into(),
                label: f.rule.title.into(),
                blurb: f.rule.blurb.into(),
                bytes: f.bytes,
                items: f.items.len() as u64,
                safety: match f.rule.safety {
                    Safety::Safe => "safe",
                    Safety::CheckFirst => "check_first",
                    Safety::UseTool => "use_tool",
                }
                .into(),
                refills: f.rule.refills,
                running: f.running,
                tool: f.rule.tool.into(),
            })
            .collect(),
    }
}

/// The report as JSON with each entry's paths added, for the command line
/// and the Unraid page, which show exactly what would go.
pub fn report_json(findings: &[Finding], now: i64) -> serde_json::Value {
    let mut value = serde_json::to_value(report(findings, now)).unwrap_or_default();
    if let Some(entries) = value
        .get_mut("entries")
        .and_then(serde_json::Value::as_array_mut)
    {
        for (entry, finding) in entries.iter_mut().zip(findings) {
            let paths: Vec<String> = finding
                .items
                .iter()
                .flat_map(|i| i.paths())
                .map(|p| p.to_string_lossy().into_owned())
                .collect();
            entry["paths"] = serde_json::json!(paths);
            // Leftovers say which program each path belonged to.
            if finding.items.iter().any(|i| !i.note.is_empty()) {
                let notes: Vec<String> = finding
                    .items
                    .iter()
                    .flat_map(|i| i.paths().into_iter().map(|_| i.note.clone()))
                    .collect();
                entry["notes"] = serde_json::json!(notes);
            }
        }
    }
    value
}

/// What cleaning one rule did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    /// Deleted for good.
    pub freed: u64,
    /// Moved to the quarantine, to be freed when it's emptied.
    pub quarantined: u64,
    /// Files that stayed, usually in use.
    pub skipped: Vec<junk::Skipped>,
}

impl Outcome {
    /// One line for the Hub's request and the status bar.
    pub fn detail(&self) -> String {
        let mut parts = Vec::new();
        if self.freed > 0 {
            parts.push(format!("freed {}", freesize_core::format_size(self.freed)));
        }
        if self.quarantined > 0 {
            parts.push(format!(
                "moved {} to the quarantine",
                freesize_core::format_size(self.quarantined)
            ));
        }
        if parts.is_empty() {
            parts.push("nothing to free".into());
        }
        if !self.skipped.is_empty() {
            parts.push(format!(
                "{} in use stayed",
                match self.skipped.len() {
                    1 => "1 file".to_string(),
                    n => format!("{n} files"),
                }
            ));
        }
        parts.join("; ")
    }
}

/// Cleans one finding. `quarantine` moves paths aside and returns the
/// bytes set aside; it's only called for "check first" rules.
pub fn clean(
    finding: &Finding,
    quarantine: &dyn Fn(&[PathBuf]) -> Result<u64, String>,
) -> Result<Outcome, String> {
    if finding.running {
        return Err(format!(
            "{} is running; close it and ask again",
            finding.rule.app
        ));
    }
    let mut outcome = Outcome::default();
    match finding.rule.safety {
        Safety::UseTool => {
            return Err(format!(
                "{} is removed with its own tool: {}",
                finding.rule.title, finding.rule.tool
            ));
        }
        Safety::Safe => {
            for item in &finding.items {
                let cleaned = junk::delete(item);
                outcome.freed += cleaned.freed;
                outcome.skipped.extend(cleaned.skipped);
            }
        }
        Safety::CheckFirst => {
            let paths: Vec<PathBuf> = finding.items.iter().flat_map(|i| i.paths()).collect();
            outcome.quarantined = quarantine(&paths)?;
        }
    }
    Ok(outcome)
}

/// The finding of one rule among this machine's, found afresh so what goes
/// is what's there now.
pub fn find_rule(platform: junk::Platform, kind: &str) -> Result<Finding, String> {
    junk::find_all(
        platform,
        &junk::Env::system(),
        &junk::running_programs(),
        junk::now(),
        freesize_core::leftovers::record_path().as_deref(),
    )
    .into_iter()
    .find(|f| f.rule.id == kind)
    .ok_or_else(|| {
        if junk::known_rule(kind) {
            "nothing of that kind to clean any more".to_string()
        } else {
            format!("this device has no junk rule \"{kind}\"")
        }
    })
}

#[cfg(test)]
mod tests {
    use freesize_core::junk::{Env, Platform};

    use super::*;

    fn fixtures(env: &Env, paths: &[&str]) {
        for path in paths {
            let path = env.expand(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"junk").unwrap();
        }
    }

    #[test]
    fn reports_and_cleans_by_safety() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::under(dir.path());
        fixtures(
            &env,
            &[
                "~/.cache/thumbnails/large/a.png",
                "~/Downloads/setup.deb",
                "$VAR/cache/apt/archives/x.deb",
            ],
        );
        let findings = junk::find(Platform::Linux, &env, &[], junk::now());
        let report = report(&findings, 7);
        assert_eq!(report.total, 12);
        assert_eq!(report.safe, 4);
        let ids: Vec<&str> = report.entries.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(ids, ["apt-cache", "thumbnails", "old-installers"]);
        assert_eq!(report.entries[0].safety, "use_tool");
        assert_eq!(report.entries[0].tool, "sudo apt clean");

        let by_id = |id: &str| findings.iter().find(|f| f.rule.id == id).unwrap();
        let no_quarantine = |_: &[PathBuf]| Err::<u64, String>("no quarantine".into());
        let cleaned = clean(by_id("thumbnails"), &no_quarantine).unwrap();
        assert_eq!(cleaned.freed, 4);
        assert_eq!(cleaned.detail(), "freed 4 B");
        assert!(!env.expand("~/.cache/thumbnails/large/a.png").exists());
        assert!(env.expand("~/.cache/thumbnails").is_dir());
        assert!(
            clean(by_id("apt-cache"), &no_quarantine)
                .unwrap_err()
                .contains("sudo apt clean")
        );
        let moved = clean(by_id("old-installers"), &|paths: &[PathBuf]| {
            assert_eq!(paths, [env.expand("~/Downloads/setup.deb")]);
            Ok(4)
        })
        .unwrap();
        assert_eq!(moved.quarantined, 4);
        assert_eq!(moved.detail(), "moved 4 B to the quarantine");
        assert!(env.expand("~/Downloads/setup.deb").exists());
    }
}
