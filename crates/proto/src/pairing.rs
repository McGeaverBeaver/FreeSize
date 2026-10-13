//! Pairing a device with the Hub. The dashboard makes a one-time code, valid
//! for a few minutes, and shows it as a link (and a QR code of the link):
//!
//! ```text
//! freesize://pair?hub=https%3A%2F%2Fhub.home.lan&code=7KQ2-M9XD-4HRT&key=ABCD-EFGH-JKMN-PQRS
//! ```
//!
//! `key` is the fingerprint of the Hub's key. A device that got the link
//! checks the Hub it reaches has that key, so a fake Hub can't take the
//! pairing over. A device that only got the address and code shows the Hub's
//! fingerprint for the person to compare with the dashboard.

use crate::keys::{normalize_code, random, sha256_hex};

/// Characters of a pairing code, without dashes: 60 bits.
const CODE_LEN: usize = 12;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PairingLink {
    /// The Hub's address, e.g. `https://hub.home.lan`.
    pub hub: String,
    pub code: String,
    /// The Hub key's fingerprint, when the link carries it.
    pub key: Option<String>,
}

/// The link's scheme, and DiskTree's from before the rename, still read
/// so links and QR codes from a Hub that isn't updated yet work.
const SCHEMES: [&str; 2] = ["freesize://", "disktree://"];

/// Whether `text` is meant as a pairing link, complete or not.
pub fn is_link(text: &str) -> bool {
    SCHEMES.iter().any(|s| text.trim_start().starts_with(s))
}

impl PairingLink {
    pub fn to_uri(&self) -> String {
        let mut uri = format!(
            "freesize://pair?hub={}&code={}",
            encode(&self.hub),
            encode(&self.code)
        );
        if let Some(key) = &self.key {
            uri.push_str(&format!("&key={}", encode(key)));
        }
        uri
    }

    /// Reads a pairing link.
    pub fn parse(text: &str) -> Option<PairingLink> {
        let text = text.trim();
        let query = SCHEMES
            .iter()
            .find_map(|scheme| text.strip_prefix(scheme)?.strip_prefix("pair?"))?;
        let (mut hub, mut code, mut key) = (None, None, None);
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=')?;
            let value = decode(value)?;
            match name {
                "hub" => hub = Some(value),
                "code" => code = Some(value),
                "key" => key = Some(value),
                _ => {}
            }
        }
        let hub = normalize_hub(&hub?)?;
        Some(PairingLink {
            hub,
            code: format_code(&code?)?,
            key,
        })
    }
}

/// A new pairing code, `XXXX-XXXX-XXXX`.
pub fn new_code() -> String {
    let text = crate::keys::crockford(&random::<8>());
    format_code(&text[..CODE_LEN]).expect("generated codes are valid")
}

/// A typed code in its canonical form, if it is one.
pub fn format_code(text: &str) -> Option<String> {
    let code = normalize_code(text);
    let valid = code.len() == CODE_LEN
        && code
            .bytes()
            .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b));
    valid.then(|| format!("{}-{}-{}", &code[..4], &code[4..8], &code[8..]))
}

/// What the Hub stores instead of the code itself.
pub fn code_hash(code: &str) -> String {
    sha256_hex(normalize_code(code).as_bytes())
}

/// Whether `hub` (as `normalize_hub` writes it) sends what a device reports
/// over the network unencrypted: plain `http://` to anything but this
/// machine. Devices warn before pairing with such a Hub.
pub fn plain_http_over_network(hub: &str) -> bool {
    let Some(rest) = hub.strip_prefix("http://") else {
        return false;
    };
    let host = rest
        .split('/')
        .next()
        .unwrap_or_default()
        .trim_start_matches('[')
        .rsplit_once(':')
        .map_or(rest.split('/').next().unwrap_or_default(), |(h, _)| h)
        .trim_end_matches(']');
    !matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
}

/// A Hub address as typed: a scheme is added if missing (`https://`), and a
/// trailing slash removed.
pub fn normalize_hub(text: &str) -> Option<String> {
    let text = text.trim();
    let (scheme, rest) = if let Some(rest) = text.strip_prefix("http://") {
        ("http", rest)
    } else if let Some(rest) = text.strip_prefix("https://") {
        ("https", rest)
    } else {
        ("https", text)
    };
    let rest = rest.trim_end_matches('/');
    let valid = !rest.is_empty() && !rest.contains(char::is_whitespace) && !rest.contains("://");
    valid.then(|| format!("{scheme}://{rest}"))
}

/// Percent-encodes everything but unreserved characters.
pub fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_round_trip() {
        let link = PairingLink {
            hub: "https://hub.home.lan:9090".into(),
            code: new_code(),
            key: Some("ABCD-EFGH-JKMN-PQRS".into()),
        };
        let uri = link.to_uri();
        assert!(uri.starts_with("freesize://pair?hub=https%3A%2F%2Fhub.home.lan%3A9090&code="));
        assert_eq!(PairingLink::parse(&uri), Some(link));
        assert_eq!(PairingLink::parse("https://example.com"), None);
        assert_eq!(PairingLink::parse("freesize://pair?hub=x"), None);
    }

    #[test]
    fn links_from_before_the_rename_still_pair() {
        let link = PairingLink {
            hub: "https://hub.home.lan".into(),
            code: new_code(),
            key: None,
        };
        let old = link.to_uri().replacen("freesize://", "disktree://", 1);
        assert!(is_link(&old) && is_link(&format!(" {}", link.to_uri())));
        assert_eq!(PairingLink::parse(&old), Some(link));
        assert!(!is_link("https://hub.home.lan"));
    }

    #[test]
    fn codes_forgive_typing() {
        let code = new_code();
        assert_eq!(code.len(), 14);
        assert_eq!(
            format_code(&code.to_lowercase().replace('-', " ")),
            Some(code.clone())
        );
        assert_eq!(format_code("7kq2m9xd4hrO"), Some("7KQ2-M9XD-4HR0".into()));
        assert_eq!(format_code("too short"), None);
        assert_eq!(code_hash("7kq2-m9xd-4hro"), code_hash("7KQ2M9XD4HR0"));
    }

    #[test]
    fn hub_addresses() {
        assert_eq!(normalize_hub("hub.lan/"), Some("https://hub.lan".into()));
        assert_eq!(
            normalize_hub("http://10.0.0.2:9090"),
            Some("http://10.0.0.2:9090".into())
        );
        assert_eq!(normalize_hub("  "), None);
        assert_eq!(normalize_hub("https://"), None);
        assert!(plain_http_over_network("http://10.0.0.2:9090"));
        assert!(plain_http_over_network("http://hub.lan"));
        assert!(!plain_http_over_network("http://localhost:9090"));
        assert!(!plain_http_over_network("http://127.0.0.1:9090"));
        assert!(!plain_http_over_network("http://[::1]:9090"));
        assert!(!plain_http_over_network("https://hub.lan"));
        assert_eq!(decode("a%20b+c"), Some("a b c".into()));
        assert_eq!(decode("%zz"), None);
    }
}
