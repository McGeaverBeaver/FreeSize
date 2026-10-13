use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use freesize_core::{
    DuplicateProgress, NodeId, ScanOptions, ScanProgress, ScanResult, SizeMode, SortKey, Tree,
    duplicate_candidates, extension_stats, flags, format_count, format_size, largest_files, ncdu,
    old_files, rebuildable, scan,
};

mod agent;
mod release;
#[cfg(unix)]
mod unraid;
mod update;

/// Shows what is using space on a drive or in a folder.
#[derive(Parser, Debug)]
#[command(version, about, args_conflicts_with_subcommands = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<agent::Command>,
    #[command(flatten)]
    scan: Args,
}

#[derive(clap::Args, Debug)]
struct Args {
    /// Folder or drive to scan.
    #[arg(default_value = ".")]
    path: PathBuf,

    /// Folder levels to print.
    #[arg(short, long, default_value_t = 2)]
    depth: usize,

    /// Hide entries smaller than this percentage of their parent folder.
    #[arg(short, long, default_value_t = 1.0)]
    min_percent: f64,

    /// Number of largest files to list.
    #[arg(short = 'n', long, default_value_t = 10)]
    top: usize,

    /// Number of file types to list.
    #[arg(short, long, default_value_t = 10)]
    types: usize,

    /// Use file sizes instead of size on disk.
    #[arg(short, long)]
    apparent: bool,

    /// Also scan other filesystems mounted inside the folder.
    #[arg(short = 'x', long)]
    cross_filesystems: bool,

    /// Scanner threads (0 = automatic).
    #[arg(short = 'j', long, default_value_t = 0)]
    threads: usize,

    /// Also list folders that tools rebuild (node_modules, target, caches…)
    /// and large files not changed in over a year. Nothing is deleted.
    #[arg(short, long)]
    cleanup: bool,

    /// Also find duplicate files of 1 MB or more. Reads the files, so it
    /// takes a while. Nothing is deleted.
    #[arg(long)]
    duplicates: bool,

    /// Print the results as JSON instead of text.
    #[arg(long)]
    json: bool,

    /// Save the scan to a file in ncdu's JSON format, to open later in
    /// FreeSize or ncdu.
    #[arg(long, value_name = "FILE")]
    export: Option<PathBuf>,

    /// Open a saved scan (FreeSize's or `ncdu -o`'s) instead of scanning.
    #[arg(long, value_name = "FILE")]
    import: Option<PathBuf>,
}

fn main() -> ExitCode {
    // Settings named for DiskTree (`DISKTREE_AGENT_STATE` in a service
    // from before the rename) count until they're renamed.
    for (key, value) in freesize_core::renamed::old_env() {
        // SAFETY: no other thread has started yet.
        unsafe { std::env::set_var(key, value) };
    }
    let cli = Cli::parse();
    match cli.command {
        Some(command) => agent::main(command),
        None => scan_and_print(cli.scan),
    }
}

fn scan_and_print(args: Args) -> ExitCode {
    let options = ScanOptions {
        threads: args.threads,
        one_filesystem: !args.cross_filesystems,
    };
    let progress = ScanProgress::new();
    let show_progress = std::io::stderr().is_terminal();

    let result = std::thread::scope(|s| {
        let handle = s.spawn(|| match &args.import {
            Some(file) => {
                let started = std::time::Instant::now();
                std::fs::File::open(file)
                    .and_then(|f| ncdu::import(std::io::BufReader::new(f)))
                    .map(|tree| ScanResult {
                        tree,
                        elapsed: started.elapsed(),
                        errors: 0,
                        cancelled: false,
                    })
            }
            None => scan(&args.path, &options, &progress),
        });
        while show_progress && !handle.is_finished() {
            if args.import.is_some() {
                eprint!("\r\x1b[2KOpening the saved scan…");
                let _ = std::io::stderr().flush();
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            let snap = progress.snapshot();
            eprint!(
                "\r\x1b[2KScanning… {} files, {} folders, {}",
                format_count(snap.files),
                format_count(snap.dirs),
                format_size(snap.bytes)
            );
            let _ = std::io::stderr().flush();
            std::thread::sleep(Duration::from_millis(100));
        }
        if show_progress {
            eprint!("\r\x1b[2K");
        }
        handle.join().expect("scanner thread panicked")
    });

    let result = match result {
        Ok(result) => result,
        Err(err) => {
            let source = args.import.as_ref().unwrap_or(&args.path);
            eprintln!("freesize: {}: {err}", source.display());
            return ExitCode::FAILURE;
        }
    };

    if let Some(file) = &args.export {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        let written = std::fs::File::create(file)
            .and_then(|f| ncdu::export(&result.tree, now, std::io::BufWriter::new(f)));
        if let Err(err) = written {
            eprintln!("freesize: {}: {err}", file.display());
            return ExitCode::FAILURE;
        }
    }

    let tree = &result.tree;
    let mode = if args.apparent {
        SizeMode::Logical
    } else {
        SizeMode::Allocated
    };
    if args.json {
        let report = json_report(tree, mode, &args, &result, show_progress);
        println!(
            "{}",
            serde_json::to_string_pretty(&report).unwrap_or_default()
        );
        return if result.cancelled {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        };
    }
    let root = tree.node(tree.root());
    let scanned = match &args.import {
        Some(file) => format!("Saved scan ({})", file.display()),
        None => format!("Scanned in {:.2}s", result.elapsed.as_secs_f64()),
    };
    println!(
        "{}: {}: {} files, {} folders{}",
        tree.name(tree.root()),
        scanned,
        format_count(u64::from(root.files)),
        format_count(u64::from(root.dirs)),
        if result.errors > 0 {
            format!(", {} unreadable", format_count(result.errors))
        } else {
            String::new()
        }
    );
    println!(
        "Total: {} ({} size on disk)\n",
        format_size(root.size),
        format_size(root.allocated)
    );
    print_unreadable(tree, args.import.is_some());

    let printer = TreePrinter {
        tree,
        mode,
        max_depth: args.depth,
        min_fraction: args.min_percent / 100.0,
    };
    printer.print_line(tree.root(), 1.0, "", "");
    printer.print_children(tree.root(), "", 1);

    if args.top > 0 {
        println!("\nLargest files");
        for id in largest_files(tree, tree.root(), mode, args.top) {
            println!(
                "  {:>9}  {}",
                format_size(tree.node(id).size_in(mode)),
                tree.path(id).display()
            );
        }
    }

    if args.types > 0 {
        println!("\nFile types");
        for stat in extension_stats(tree, tree.root(), mode)
            .into_iter()
            .take(args.types)
        {
            let name = if stat.extension.is_empty() {
                "(none)".to_string()
            } else {
                format!(".{}", stat.extension)
            };
            println!(
                "  {:>9}  {:<12} {:>10} files",
                format_size(stat.size),
                name,
                format_count(stat.files)
            );
        }
    }

    if args.cleanup {
        print_cleanup(tree, mode);
    }
    if args.duplicates {
        print_duplicates(tree, show_progress);
    }

    if result.cancelled {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

const MB: u64 = 1024 * 1024;

/// Names the folders the scan couldn't list, and how to read them.
fn print_unreadable(tree: &Tree, imported: bool) {
    let missed = tree.unreadable();
    if missed.is_empty() {
        return;
    }
    println!(
        "Couldn't read {} folder{}:",
        format_count(missed.len() as u64),
        if missed.len() == 1 { "" } else { "s" }
    );
    for &id in missed.iter().take(10) {
        println!("  {}", tree.path(id).display());
    }
    if missed.len() > 10 {
        println!("  … and {} more", format_count(missed.len() as u64 - 10));
    }
    if !imported && !freesize_core::access::rights().sees_everything() {
        println!(
            "{}",
            if cfg!(windows) {
                "Run freesize from an administrator prompt to read them; System Volume \
                 Information stays closed even then."
            } else {
                "Run it with sudo to read them, or `sudo freesize PATH --export scan.json` and \
                 open the file in the app."
            }
        );
    }
    println!();
}

/// Everything the text output shows, as JSON.
fn json_report(
    tree: &Tree,
    mode: SizeMode,
    args: &Args,
    result: &ScanResult,
    show_progress: bool,
) -> serde_json::Value {
    use serde_json::json;
    let root = tree.node(tree.root());
    let entry = |id: NodeId| {
        let node = tree.node(id);
        json!({
            "name": tree.name(id),
            "kind": if node.is_dir() { "folder" } else { "file" },
            "size": node.size,
            "size_on_disk": node.allocated,
            "files": node.files,
        })
    };
    let mut report = json!({
        "path": tree.path(tree.root()),
        "size": root.size,
        "size_on_disk": root.allocated,
        "files": root.files,
        "folders": root.dirs,
        "unreadable": result.errors,
        "unreadable_folders": tree
            .unreadable()
            .into_iter()
            .take(100)
            .map(|id| tree.path(id))
            .collect::<Vec<_>>(),
        "rights": freesize_core::access::rights().label(),
        "seconds": result.elapsed.as_secs_f64(),
        "complete": !result.cancelled,
        "contents": tree
            .children_sorted(tree.root(), SortKey::Size(mode))
            .into_iter()
            .map(entry)
            .collect::<Vec<_>>(),
        "largest_files": largest_files(tree, tree.root(), mode, args.top)
            .into_iter()
            .map(|id| json!({"path": tree.path(id), "size": tree.node(id).size_in(mode)}))
            .collect::<Vec<_>>(),
        "types": extension_stats(tree, tree.root(), mode)
            .into_iter()
            .take(args.types)
            .map(|s| json!({"extension": s.extension, "size": s.size, "files": s.files}))
            .collect::<Vec<_>>(),
    });
    if args.cleanup {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64);
        report["rebuildable"] = rebuildable(tree, tree.root())
            .into_iter()
            .map(|r| {
                json!({"path": tree.path(r.node), "what": r.what, "how": r.how,
                       "size": tree.node(r.node).size_in(mode)})
            })
            .collect();
        report["old_files"] = old_files(tree, tree.root(), mode, now - YEAR, 20)
            .into_iter()
            .map(|id| {
                json!({"path": tree.path(id), "size": tree.node(id).size_in(mode),
                             "modified": tree.node(id).modified})
            })
            .collect();
    }
    if args.duplicates {
        report["duplicates"] = find_groups(tree, show_progress)
            .into_iter()
            .map(|g| {
                json!({"size": g.size,
                       "paths": g.nodes.iter().map(|&id| tree.path(id)).collect::<Vec<_>>()})
            })
            .collect();
    }
    report
}

const YEAR: i64 = 365 * 24 * 60 * 60;

fn print_cleanup(tree: &Tree, mode: SizeMode) {
    let mut folders: Vec<_> = rebuildable(tree, tree.root())
        .into_iter()
        .filter(|r| tree.node(r.node).size_in(mode) >= MB)
        .collect();
    folders.sort_by_key(|r| std::cmp::Reverse(tree.node(r.node).size_in(mode)));
    let total: u64 = folders
        .iter()
        .map(|r| tree.node(r.node).size_in(mode))
        .sum();
    println!("\nRebuildable folders: {}", format_size(total));
    for r in &folders {
        println!(
            "  {:>9}  {}  ({}; {})",
            format_size(tree.node(r.node).size_in(mode)),
            tree.path(r.node).display(),
            r.what,
            r.how
        );
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    println!("\nNot changed in over a year");
    for id in old_files(tree, tree.root(), mode, now - YEAR, 20) {
        let size = tree.node(id).size_in(mode);
        if size >= 10 * MB {
            println!("  {:>9}  {}", format_size(size), tree.path(id).display());
        }
    }
}

fn print_duplicates(tree: &Tree, show_progress: bool) {
    let groups = find_groups(tree, show_progress);
    print_groups(tree, &groups);
}

fn find_groups(tree: &Tree, show_progress: bool) -> Vec<freesize_core::DuplicateGroup> {
    let candidates = duplicate_candidates(tree, tree.root(), MB);
    let progress = DuplicateProgress::new();
    std::thread::scope(|s| {
        let handle = s.spawn(|| candidates.find(&progress));
        while show_progress && !handle.is_finished() {
            eprint!(
                "\r\x1b[2KComparing files… {} of {}",
                format_count(progress.done.load(std::sync::atomic::Ordering::Relaxed)),
                format_count(progress.files.load(std::sync::atomic::Ordering::Relaxed))
            );
            let _ = std::io::stderr().flush();
            std::thread::sleep(Duration::from_millis(100));
        }
        if show_progress {
            eprint!("\r\x1b[2K");
        }
        handle.join().expect("duplicate finder panicked")
    })
}

fn print_groups(tree: &Tree, groups: &[freesize_core::DuplicateGroup]) {
    let wasted: u64 = groups.iter().map(|g| g.wasted()).sum();
    println!(
        "\nDuplicate files: {} set{}, {} in extra copies",
        format_count(groups.len() as u64),
        if groups.len() == 1 { "" } else { "s" },
        format_size(wasted)
    );
    for group in groups {
        println!(
            "  {} copies of {}",
            group.nodes.len(),
            format_size(group.size)
        );
        for &id in &group.nodes {
            println!("    {}", tree.path(id).display());
        }
    }
}

struct TreePrinter<'a> {
    tree: &'a Tree,
    mode: SizeMode,
    max_depth: usize,
    min_fraction: f64,
}

impl TreePrinter<'_> {
    fn print_children(&self, dir: NodeId, prefix: &str, depth: usize) {
        if depth > self.max_depth {
            return;
        }
        let tree = self.tree;
        let total = tree.node(dir).size_in(self.mode);
        let children = tree.children_sorted(dir, SortKey::Size(self.mode));
        let fraction = |id: NodeId| {
            if total == 0 {
                0.0
            } else {
                tree.node(id).size_in(self.mode) as f64 / total as f64
            }
        };
        let shown = children
            .iter()
            .take_while(|&&c| fraction(c) >= self.min_fraction && fraction(c) > 0.0)
            .count();
        let hidden = &children[shown..];

        for (i, &child) in children[..shown].iter().enumerate() {
            let last = i + 1 == shown && hidden.is_empty();
            let (branch, indent) = if last {
                ("└── ", "    ")
            } else {
                ("├── ", "│   ")
            };
            self.print_line(child, fraction(child), prefix, branch);
            if tree.node(child).is_dir() {
                self.print_children(child, &format!("{prefix}{indent}"), depth + 1);
            }
        }

        if !hidden.is_empty() {
            let size: u64 = hidden
                .iter()
                .map(|&c| tree.node(c).size_in(self.mode))
                .sum();
            let frac = if total == 0 {
                0.0
            } else {
                size as f64 / total as f64
            };
            println!(
                "{:>9} {:>6.1}%  {}  {prefix}└── … {} smaller items",
                format_size(size),
                frac * 100.0,
                bar(frac, 20),
                format_count(hidden.len() as u64)
            );
        }
    }

    fn print_line(&self, id: NodeId, fraction: f64, prefix: &str, branch: &str) {
        let tree = self.tree;
        let node = tree.node(id);
        let mut name = tree.name(id).into_owned();
        if node.is_dir() && id != tree.root() {
            name.push(std::path::MAIN_SEPARATOR);
        }
        let mut notes = Vec::new();
        if node.has(flags::ERROR) {
            notes.push("unreadable");
        }
        if node.has(flags::MOUNT_POINT) {
            notes.push("other filesystem");
        }
        if node.has(flags::CLOUD) {
            notes.push("in the cloud");
        }
        if node.has(flags::HARDLINK_DUP) {
            notes.push("hard link");
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!("  [{}]", notes.join(", "))
        };
        println!(
            "{:>9} {:>6.1}%  {}  {prefix}{branch}{name}{notes}",
            format_size(node.size_in(self.mode)),
            fraction * 100.0,
            bar(fraction, 20)
        );
    }
}

/// A bar of `width` cells with eighth-cell resolution.
fn bar(fraction: f64, width: usize) -> String {
    const PARTS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];
    let eighths = (fraction.clamp(0.0, 1.0) * width as f64 * 8.0).round() as usize;
    let full = eighths / 8;
    let mut out = "█".repeat(full);
    if full < width {
        out.push(PARTS[eighths % 8]);
        out.push_str(&" ".repeat(width - full - 1));
    }
    out
}
