//! `freesize review`: a folder at a glance before deleting it, such as an
//! old backup of a PC. Its files by what they are, the ones worth a look
//! first, which contain words like "tax" or "passport" (only the word is
//! named, never the text), and, with `--in`, which are kept elsewhere too.
//! Nothing is deleted. On a server it's how the review runs where the
//! files are.

use std::collections::BTreeMap;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use clap::Args;
use freesize_agent::review as contents;
use freesize_core::review::{self, Group};
use freesize_core::{
    DuplicateProgress, NodeId, ScanOptions, ScanProgress, SizeMode, Tree, format_count,
    format_size, scan,
};
use serde::Serialize;

#[derive(Args, Debug)]
pub struct ReviewArgs {
    /// The folder to review.
    pub folder: PathBuf,
    /// A bigger folder holding it (a whole drive, a share): files of the
    /// reviewed folder also found here, by content, are counted as kept
    /// elsewhere.
    #[arg(long = "in", value_name = "FOLDER")]
    pub within: Option<PathBuf>,
    /// Words to look for inside text, Office and PDF files, separated by
    /// commas.
    #[arg(long, value_name = "WORDS", default_value_t = contents::DEFAULT_WORDS.join(","))]
    pub words: String,
    /// Don't look inside files.
    #[arg(long)]
    pub no_words: bool,
    /// Print the review as JSON.
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize, Default)]
struct Report {
    folder: String,
    files: u64,
    bytes: u64,
    groups: BTreeMap<&'static str, Count>,
    /// Paths by why they're worth a look.
    important: BTreeMap<&'static str, Vec<String>>,
    /// Paths and the words found in each.
    words: Vec<(String, Vec<String>)>,
    /// With `--in`: how many are kept elsewhere, and the paths that aren't.
    kept_elsewhere: Option<u64>,
    only_here: Vec<String>,
}

#[derive(Serialize, Default)]
struct Count {
    files: u64,
    bytes: u64,
}

pub fn main(args: ReviewArgs) -> Result<(), String> {
    let folder = std::fs::canonicalize(&args.folder)
        .map_err(|err| format!("{}: {err}", args.folder.display()))?;
    let within = match &args.within {
        Some(dir) => {
            std::fs::canonicalize(dir).map_err(|err| format!("{}: {err}", dir.display()))?
        }
        None => folder.clone(),
    };
    let relative = folder
        .strip_prefix(&within)
        .map_err(|_| format!("{} isn't inside {}", folder.display(), within.display()))?
        .to_path_buf();
    let chatty = std::io::stderr().is_terminal() && !args.json;
    if chatty {
        eprintln!("Scanning {}…", within.display());
    }
    let tree = scan(&within, &ScanOptions::default(), &ScanProgress::new())
        .map_err(|err| format!("{}: {err}", within.display()))?
        .tree;
    let node = find(&tree, &relative)
        .ok_or_else(|| format!("{} wasn't found in the scan", folder.display()))?;

    let mode = SizeMode::Allocated;
    let files = review::files(&tree, node, mode);
    let mut report = Report {
        folder: folder.display().to_string(),
        files: files.len() as u64,
        bytes: tree.node(node).size_in(mode),
        ..Report::default()
    };
    for &id in &files {
        let name = tree.name(id);
        let path = tree.path(id);
        let count = report
            .groups
            .entry(review::group_of(&name).key())
            .or_default();
        count.files += 1;
        count.bytes += tree.node(id).size_in(mode);
        if let Some(why) = review::important(&name, &path.to_string_lossy()) {
            report
                .important
                .entry(why)
                .or_default()
                .push(path.display().to_string());
        }
    }

    if !args.no_words {
        let words = contents::parse_words(&args.words);
        let searchable: Vec<PathBuf> = files
            .iter()
            .map(|&id| tree.path(id))
            .filter(|p| contents::searchable(p))
            .collect();
        if chatty && !searchable.is_empty() {
            eprintln!(
                "Looking inside {} files…",
                format_count(searchable.len() as u64)
            );
        }
        for path in searchable {
            let found = contents::words_in(&path, &words);
            if !found.is_empty() {
                report.words.push((path.display().to_string(), found));
            }
        }
    }

    if args.within.is_some() {
        let candidates = review::copy_candidates(&tree, node);
        if chatty && candidates.files() > 0 {
            eprintln!(
                "Comparing {} files with the rest of {}…",
                format_count(candidates.files() as u64),
                within.display()
            );
        }
        let copies = candidates.find(&DuplicateProgress::new());
        report.kept_elsewhere = Some(copies.len() as u64);
        report.only_here = files
            .iter()
            .filter(|id| !copies.contains_key(id))
            .map(|&id| tree.path(id).display().to_string())
            .collect();
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
        );
    } else {
        print_report(&report);
    }
    Ok(())
}

/// The node at `relative` below the tree's root.
fn find(tree: &Tree, relative: &Path) -> Option<NodeId> {
    let mut id = tree.root();
    for part in relative.components() {
        id = tree.child_by_name(id, part.as_os_str())?;
    }
    Some(id)
}

fn print_report(report: &Report) {
    println!(
        "{}: {} files, {}",
        report.folder,
        format_count(report.files),
        format_size(report.bytes)
    );
    for group in Group::ALL {
        let count = report.groups.get(group.key());
        let label = match group {
            Group::Photos => "Photos",
            Group::Documents => "Documents",
            Group::Videos => "Videos",
            Group::Other => "Everything else",
        };
        let (files, bytes) = count.map_or((0, 0), |c| (c.files, c.bytes));
        println!(
            "  {label:<16} {:>8} files  {:>10}",
            format_count(files),
            format_size(bytes)
        );
    }
    if !report.important.is_empty() {
        println!("\nWorth a look before deleting this folder:");
        for (why, paths) in &report.important {
            println!("  {why} ({})", format_count(paths.len() as u64));
            for path in paths.iter().take(10) {
                println!("    {path}");
            }
            if paths.len() > 10 {
                println!("    … and {} more", format_count(paths.len() as u64 - 10));
            }
        }
    }
    if !report.words.is_empty() {
        println!("\nFiles containing the words looked for:");
        for (path, words) in &report.words {
            println!("  {path}  ({})", words.join(", "));
        }
    }
    if let Some(kept) = report.kept_elsewhere {
        let percent = (kept * 100).checked_div(report.files).unwrap_or(0);
        println!(
            "\n{} of {} files ({percent}%) are kept elsewhere too.",
            format_count(kept),
            format_count(report.files)
        );
        if !report.only_here.is_empty() {
            println!("Only here:");
            for path in report.only_here.iter().take(50) {
                println!("  {path}");
            }
            if report.only_here.len() > 50 {
                println!(
                    "  … and {} more (--json lists them all)",
                    format_count(report.only_here.len() as u64 - 50)
                );
            }
        }
    }
}
