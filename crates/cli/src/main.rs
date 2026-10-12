use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use clap::Parser;
use disktree_core::{
    DuplicateProgress, NodeId, ScanOptions, ScanProgress, SizeMode, SortKey, Tree,
    duplicate_candidates, extension_stats, flags, format_count, format_size, largest_files,
    old_files, rebuildable, scan,
};

/// Shows what is using space on a drive or in a folder.
#[derive(Parser, Debug)]
#[command(version, about)]
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
}

fn main() -> ExitCode {
    let args = Args::parse();
    let options = ScanOptions {
        threads: args.threads,
        one_filesystem: !args.cross_filesystems,
    };
    let progress = ScanProgress::new();
    let show_progress = std::io::stderr().is_terminal();

    let result = std::thread::scope(|s| {
        let handle = s.spawn(|| scan(&args.path, &options, &progress));
        while show_progress && !handle.is_finished() {
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
            eprintln!("disktree: {}: {err}", args.path.display());
            return ExitCode::FAILURE;
        }
    };

    let tree = &result.tree;
    let mode = if args.apparent {
        SizeMode::Logical
    } else {
        SizeMode::Allocated
    };
    let root = tree.node(tree.root());
    println!(
        "Scanned {} in {:.2}s: {} files, {} folders{}",
        tree.name(tree.root()),
        result.elapsed.as_secs_f64(),
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
    let candidates = duplicate_candidates(tree, tree.root(), MB);
    let progress = DuplicateProgress::new();
    let groups = std::thread::scope(|s| {
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
    });
    let wasted: u64 = groups.iter().map(|g| g.wasted()).sum();
    println!(
        "\nDuplicate files: {} sets, {} in extra copies",
        format_count(groups.len() as u64),
        format_size(wasted)
    );
    for group in &groups {
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
