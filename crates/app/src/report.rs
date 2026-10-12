//! Reports of a scanned folder to save or share: an HTML page to read, and a
//! CSV table for spreadsheets.

use std::fmt::Write;

use disktree_core::{
    NodeId, SizeMode, SortKey, Tree, age_bins, extension_stats, flags, format_count, format_size,
    largest_files,
};

use crate::CleanRow;

/// What goes into a report besides the tree.
pub struct Extras<'a> {
    /// The scanned folder's name, e.g. "Internal storage".
    pub title: &'a str,
    pub now: i64,
    /// Rows of the Clean up and Changes tabs.
    pub cleanup: &'a [CleanRow],
    pub changes: &'a [CleanRow],
}

/// `yyyy-mm-dd` of a time in seconds since 1970.
pub fn iso_date(secs: i64) -> String {
    // Howard Hinnant's civil-from-days.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const STYLE: &str = "\
:root{--fg:#1b1b1f;--dim:#6b6b75;--bar:#4f8ff7;--track:#0000001a;--bg:#fff}\
@media (prefers-color-scheme:dark){:root{--fg:#e8e8ec;--dim:#9a9aa5;--track:#ffffff1f;--bg:#1c1c1f}}\
body{font:15px/1.45 system-ui,sans-serif;color:var(--fg);background:var(--bg);margin:0 auto;\
max-width:880px;padding:24px 16px}h1{font-size:22px;margin:0}h2{font-size:17px;margin:28px 0 6px}\
.dim{color:var(--dim)}table{width:100%;border-collapse:collapse}td{padding:4px 6px;\
vertical-align:top;border-bottom:1px solid var(--track)}td.n{text-align:right;white-space:nowrap}\
.bar{height:8px;background:var(--track);border-radius:3px;min-width:80px}\
.bar>div{height:8px;background:var(--bar);border-radius:3px}.sub{font-size:12px;color:var(--dim)}";

/// A page about the folder `focus`: its biggest folders and files, file
/// types, ages, clean-up suggestions and changes.
pub fn html(tree: &Tree, focus: NodeId, mode: SizeMode, extras: &Extras) -> String {
    let node = tree.node(focus);
    let total = node.size_in(mode).max(1);
    let mut page = String::new();
    let _ = write!(
        page,
        "<!doctype html><html><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>DiskTree: {title}</title><style>{STYLE}</style></head><body>\
         <h1>{title}</h1><p class=\"dim\">{path}<br>{size} in {files} files and {dirs} folders, \
         scanned {date} by DiskTree</p>",
        title = escape(extras.title),
        path = escape(&tree.path(focus).to_string_lossy()),
        size = format_size(node.size_in(mode)),
        files = format_count(u64::from(node.files)),
        dirs = format_count(u64::from(node.dirs)),
        date = iso_date(extras.now),
    );

    let bar = |part: u64| {
        format!(
            "<div class=\"bar\"><div style=\"width:{:.1}%\"></div></div>",
            part as f64 * 100.0 / total as f64
        )
    };
    page.push_str("<h2>Biggest folders and files</h2><table>");
    for child in tree
        .children_sorted(focus, SortKey::Size(mode))
        .into_iter()
        .take(40)
    {
        let size = tree.node(child).size_in(mode);
        if size == 0 {
            break;
        }
        let folder = if tree.node(child).is_dir() { "/" } else { "" };
        let _ = write!(
            page,
            "<tr><td>{}{folder}</td><td>{}</td><td class=\"n\">{}</td></tr>",
            escape(&tree.name(child)),
            bar(size),
            format_size(size)
        );
    }
    page.push_str("</table><h2>Largest files</h2><table>");
    let top = tree.path(focus);
    for id in largest_files(tree, focus, mode, 50) {
        let size = tree.node(id).size_in(mode);
        // Where the file is, below the folder the report is about.
        let place = tree.path(tree.node(id).parent);
        let place = match place.strip_prefix(&top) {
            Ok(rest) if rest.as_os_str().is_empty() => extras.title.to_string(),
            Ok(rest) => rest.to_string_lossy().into_owned(),
            Err(_) => place.to_string_lossy().into_owned(),
        };
        let _ = write!(
            page,
            "<tr><td>{}<div class=\"sub\">{}</div></td><td class=\"n\">{}</td></tr>",
            escape(&tree.name(id)),
            escape(&place),
            format_size(size)
        );
    }
    page.push_str("</table><h2>File types</h2><table>");
    for stat in extension_stats(tree, focus, mode).into_iter().take(25) {
        let label = if stat.extension.is_empty() {
            "(no extension)".to_string()
        } else {
            format!(".{}", stat.extension)
        };
        let _ = write!(
            page,
            "<tr><td>{}</td><td class=\"n\">{} files</td><td>{}</td><td class=\"n\">{}</td></tr>",
            escape(&label),
            format_count(stat.files),
            bar(stat.size),
            format_size(stat.size)
        );
    }
    page.push_str("</table><h2>By year changed</h2><table>");
    for bin in age_bins(tree, focus, mode, extras.now) {
        let _ = write!(
            page,
            "<tr><td>{}</td><td class=\"n\">{} files</td><td>{}</td><td class=\"n\">{}</td></tr>",
            escape(&bin.label),
            format_count(bin.files),
            bar(bin.size),
            format_size(bin.size)
        );
    }
    page.push_str("</table>");
    for (title, rows) in [("Clean up", extras.cleanup), ("Changes", extras.changes)] {
        if rows.is_empty() {
            continue;
        }
        let _ = write!(page, "<h2>{title}</h2><table>");
        for row in rows {
            if row.kind == 0 {
                let _ = write!(
                    page,
                    "<tr><td colspan=\"2\"><b>{}</b><div class=\"sub\">{}</div></td></tr>",
                    escape(&row.title),
                    escape(&row.subtitle)
                );
            } else if row.kind == 1 {
                let _ = write!(
                    page,
                    "<tr><td>{}<div class=\"sub\">{}</div></td><td class=\"n\">{}</td></tr>",
                    escape(&row.title),
                    escape(&row.subtitle),
                    escape(&row.detail)
                );
            }
        }
        page.push_str("</table>");
    }
    page.push_str("</body></html>\n");
    page
}

fn csv_field(text: &str) -> String {
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text.to_string()
    }
}

/// Folders and files most worth looking at, one per line: the 10,000 biggest
/// folders and the 10,000 biggest files of at least 1 MB.
pub fn csv(tree: &Tree, focus: NodeId) -> String {
    const MAX: usize = 10_000;
    let mut out = String::from("path,kind,size,size_on_disk,files,modified\n");
    let mut dirs: Vec<NodeId> = std::iter::once(focus)
        .chain(tree.descendants(focus))
        .filter(|&id| tree.node(id).is_dir())
        .collect();
    dirs.sort_by_key(|&id| std::cmp::Reverse(tree.node(id).allocated));
    dirs.truncate(MAX);
    let files: Vec<NodeId> = largest_files(tree, focus, SizeMode::Allocated, MAX)
        .into_iter()
        .filter(|&id| tree.node(id).size >= 1024 * 1024 && !tree.node(id).has(flags::DELETED))
        .collect();
    for (id, kind) in dirs
        .iter()
        .map(|&id| (id, "folder"))
        .chain(files.iter().map(|&id| (id, "file")))
    {
        let node = tree.node(id);
        let _ = writeln!(
            out,
            "{},{kind},{},{},{},{}",
            csv_field(&tree.path(id).to_string_lossy()),
            node.size,
            node.allocated,
            node.files,
            if node.modified > 0 {
                iso_date(node.modified)
            } else {
                String::new()
            }
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use disktree_core::{ScanOptions, ScanProgress, scan};

    use super::*;

    #[test]
    fn writes_html_and_csv() {
        let dir = tempfile_dir();
        fs::create_dir_all(dir.join("a <b>")).unwrap();
        fs::write(dir.join("a <b>/big, file.bin"), vec![1u8; 2 * 1024 * 1024]).unwrap();
        fs::write(dir.join("small.txt"), b"hi").unwrap();
        let tree = scan(&dir, &ScanOptions::default(), &ScanProgress::new())
            .unwrap()
            .tree;
        let extras = Extras {
            title: "Test & co",
            now: 1_791_763_200,
            cleanup: &[],
            changes: &[],
        };
        let page = html(&tree, tree.root(), SizeMode::Logical, &extras);
        assert!(page.contains("<h1>Test &amp; co</h1>"));
        assert!(page.contains("a &lt;b&gt;/"));
        assert!(page.contains("scanned 2026-10-12"));
        assert!(page.contains("<div class=\"sub\">a &lt;b&gt;</div>"));
        let table = csv(&tree, tree.root());
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(lines[0], "path,kind,size,size_on_disk,files,modified");
        assert!(lines.iter().any(|l| l.contains(",folder,")));
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with('"') && l.contains("big, file.bin\",file,"))
        );
        assert!(!table.contains("small.txt"));
        let _ = fs::remove_dir_all(&dir);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("disktree-report-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}
