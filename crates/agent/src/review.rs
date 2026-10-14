//! Looking for words inside files, for a folder review: plain text, Office
//! and OpenDocument files (zipped XML) and PDFs. It runs on the device that
//! has the files and only says which words a file contains, never the text
//! around them, so nothing of what's inside leaves the device.

use std::fs::File;
use std::io::Read;
use std::path::Path;

/// The words looked for until the person sets their own.
pub const DEFAULT_WORDS: &[&str] = &["tax", "invoice", "passport", "bank", "contract", "password"];

/// Bigger files are skipped: reading them takes long and they're rarely
/// letters or forms.
const MAX_TEXT: u64 = 20 * 1024 * 1024;
const MAX_PDF: u64 = 100 * 1024 * 1024;
/// How much XML an Office file may unpack to.
const MAX_XML: u64 = 64 * 1024 * 1024;

const TEXT: &[&str] = &[
    "txt", "md", "csv", "tsv", "log", "json", "xml", "html", "htm", "rtf", "ini", "cfg", "conf",
    "yaml", "yml", "eml", "tex", "srt", "vcf", "ics",
];
const ZIPPED: &[&str] = &["docx", "xlsx", "pptx", "odt", "ods", "odp", "docm", "xlsm"];

fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Whether FreeSize can look inside a file like this.
pub fn searchable(path: &Path) -> bool {
    let ext = extension(path);
    TEXT.contains(&ext.as_str()) || ZIPPED.contains(&ext.as_str()) || ext == "pdf"
}

/// Words from a comma-separated list, trimmed, lower case, without empty
/// ones or repeats.
pub fn parse_words(list: &str) -> Vec<String> {
    let mut words: Vec<String> = Vec::new();
    for word in list.split(',').map(|w| w.trim().to_lowercase()) {
        if !word.is_empty() && !words.contains(&word) {
            words.push(word);
        }
    }
    words
}

/// Which of `words` (lower case) the file contains, in their order. A word
/// counts where it starts a word in the text ("tax" in "taxes", not in
/// "syntax"). Files FreeSize can't read, or too big, give none.
pub fn words_in(path: &Path, words: &[String]) -> Vec<String> {
    if words.is_empty() {
        return Vec::new();
    }
    let Some(text) = text_of(path) else {
        return Vec::new();
    };
    let text = text.to_lowercase();
    words
        .iter()
        .filter(|w| starts_a_word(&text, w))
        .cloned()
        .collect()
}

fn starts_a_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        text[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
    })
}

/// The file's text, as well as FreeSize can get it.
fn text_of(path: &Path) -> Option<String> {
    let ext = extension(path);
    let size = std::fs::metadata(path).ok()?.len();
    if TEXT.contains(&ext.as_str()) {
        if size > MAX_TEXT {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        return Some(match ext.as_str() {
            "html" | "htm" | "xml" => strip_tags(&String::from_utf8_lossy(&bytes)),
            _ => String::from_utf8_lossy(&bytes).into_owned(),
        });
    }
    if ZIPPED.contains(&ext.as_str()) {
        return zipped_text(path);
    }
    if ext == "pdf" {
        if size > MAX_PDF {
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        // The PDF reader panics on some broken files; one bad file mustn't
        // end the review.
        return std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes).ok())
            .ok()
            .flatten();
    }
    None
}

/// The text of every XML part of an Office or OpenDocument file.
fn zipped_text(path: &Path) -> Option<String> {
    let mut archive = zip::ZipArchive::new(File::open(path).ok()?).ok()?;
    let mut text = String::new();
    let mut unpacked = 0u64;
    for i in 0..archive.len() {
        let Ok(mut part) = archive.by_index(i) else {
            continue;
        };
        if !part.name().is_ok_and(|n| n.ends_with(".xml")) {
            continue;
        }
        let mut xml = String::new();
        let limit = MAX_XML.saturating_sub(unpacked);
        if limit == 0 {
            break;
        }
        if (&mut part).take(limit).read_to_string(&mut xml).is_err() {
            continue;
        }
        unpacked += xml.len() as u64;
        text.push_str(&strip_tags(&xml));
        text.push('\n');
    }
    Some(text)
}

/// Markup without its tags, with the common entities decoded. Tags become
/// a space, so words from neighbouring cells don't run together.
fn strip_tags(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len() / 2);
    let mut in_tag = false;
    for c in markup.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    /// A one-page PDF saying `text`, with a correct cross-reference table.
    fn tiny_pdf(text: &str) -> Vec<u8> {
        let content = format!("BT /F1 12 Tf 20 100 Td ({text}) Tj ET");
        let objects = [
            "<</Type/Catalog/Pages 2 0 R>>".to_string(),
            "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R\
             /Resources<</Font<</F1 5 0 R>>>>>>"
                .to_string(),
            format!(
                "<</Length {}>>\nstream\n{content}\nendstream",
                content.len()
            ),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n{object}\nendobj\n", i + 1).bytes());
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).bytes());
        for offset in offsets {
            out.extend(format!("{offset:010} 00000 n \n").bytes());
        }
        out.extend(
            format!(
                "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .bytes(),
        );
        out
    }

    #[test]
    fn finds_words_in_text_and_office_files() {
        let dir = tempfile::tempdir().unwrap();
        let note = dir.path().join("note.txt");
        std::fs::write(&note, "My Passport number and the syntax of TAXES.").unwrap();
        assert_eq!(
            words_in(&note, &words(&["tax", "passport", "bank"])),
            ["tax", "passport"]
        );
        assert_eq!(
            words_in(&note, &words(&["syntax"])),
            ["syntax"],
            "a word at the start of a word"
        );
        assert!(words_in(&note, &words(&["ntax"])).is_empty());

        // A Word file is zipped XML; the word is split by tags.
        let docx = dir.path().join("letter.docx");
        let mut zip = zip::ZipWriter::new(File::create(&docx).unwrap());
        zip.start_file(
            "word/document.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.write_all(b"<w:p><w:t>Bank</w:t><w:t>statement &amp; Invoice</w:t></w:p>")
            .unwrap();
        zip.finish().unwrap();
        assert_eq!(
            words_in(&docx, &words(&["invoice", "bank", "contract"])),
            ["invoice", "bank"]
        );

        // A small real PDF.
        let pdf = dir.path().join("statement.pdf");
        std::fs::write(&pdf, tiny_pdf("Bank statement")).unwrap();
        assert_eq!(words_in(&pdf, &words(&["bank", "tax"])), ["bank"]);

        // A broken PDF gives nothing rather than stopping.
        let pdf = dir.path().join("broken.pdf");
        std::fs::write(&pdf, b"%PDF-1.4 not really").unwrap();
        assert!(words_in(&pdf, &words(&["bank"])).is_empty());
        assert!(searchable(&pdf) && searchable(&docx) && !searchable(Path::new("a.exe")));
        assert_eq!(parse_words(" Tax, bank,,tax "), ["tax", "bank"]);
    }
}
