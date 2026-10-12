//! Unraid's settings files: `/var/local/emhttp/*.ini`, with `["section"]`
//! headers and `key="value"` lines, and `/boot/config/*.cfg`, the same
//! lines without sections.

use std::collections::BTreeMap;

/// Sections in file order, each with its keys.
pub type Sections = Vec<(String, BTreeMap<String, String>)>;

/// Reads `key="value"` lines; quotes are optional, and `#` lines are
/// comments.
pub fn parse_cfg(text: &str) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = pair(line) {
            map.insert(key, value);
        }
    }
    map
}

/// Reads `["name"]` (or `[name]`) sections and their lines.
pub fn parse_ini(text: &str) -> Sections {
    let mut sections: Sections = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            sections.push((unquote(name).to_string(), BTreeMap::new()));
        } else if let Some((key, value)) = pair(line)
            && let Some((_, map)) = sections.last_mut()
        {
            map.insert(key, value);
        }
    }
    sections
}

fn pair(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    if key.is_empty() || key.contains(char::is_whitespace) {
        return None;
    }
    Some((key.to_string(), unquote(value.trim()).to_string()))
}

fn unquote(text: &str) -> &str {
    text.strip_prefix('"')
        .and_then(|t| t.strip_suffix('"'))
        .unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_kinds() {
        let ini = parse_ini("[\"disk1\"]\nname=\"disk1\"\nfsSize=\"100\"\n[cache]\nname=cache\n");
        assert_eq!(ini.len(), 2);
        assert_eq!(ini[0].0, "disk1");
        assert_eq!(ini[0].1["fsSize"], "100");
        assert_eq!(ini[1].1["name"], "cache");
        let cfg =
            parse_cfg("# comment\nDOCKER_ENABLED=\"yes\"\nDOCKER_IMAGE_TYPE=folder\nbad line\n");
        assert_eq!(cfg["DOCKER_ENABLED"], "yes");
        assert_eq!(cfg["DOCKER_IMAGE_TYPE"], "folder");
        assert_eq!(cfg.len(), 2);
    }
}
