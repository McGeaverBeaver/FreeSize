//! Signed releases. `scripts/release.sh` writes `SHA256SUMS.txt` for the
//! release's files and signs it with the release key (`freesize release
//! sign`); the apps carry the public half, from `release/public-key.txt`
//! at build time, and refuse an update whose sums file doesn't verify. A
//! build made without the key in that file carries none and updates
//! nothing.

use crate::keys::{KeyPair, PublicKey, sha256_hex};

/// The release key's public half, as the repository holds it (base64, or
/// empty before the owner made one).
const PUBLIC_KEY_FILE: &str = include_str!("../../../release/public-key.txt");

/// What a signature covers, so a release signature can't stand in for a
/// device's.
const DOMAIN: &[u8] = b"freesize-release-v1\n";

/// The key this build trusts releases from, if it has one.
pub fn release_key() -> Option<PublicKey> {
    public_key_in(PUBLIC_KEY_FILE)
}

/// The key in a `public-key.txt`: the first line that isn't a comment.
pub fn public_key_in(text: &str) -> Option<PublicKey> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .and_then(PublicKey::from_base64)
}

/// Signs a sums file: the signature, base64, to store beside it as
/// `SHA256SUMS.txt.sig`.
pub fn sign_sums(key: &KeyPair, sums: &[u8]) -> String {
    key.sign(&[DOMAIN, sums].concat())
}

/// Whether `signature` is `key`'s signature of this sums file.
pub fn verify_sums(key: &PublicKey, sums: &[u8], signature: &str) -> bool {
    key.verify(&[DOMAIN, sums].concat(), signature.trim())
}

/// The entries of a `sha256sum` file: `(hex hash, file name)`. Names may be
/// marked binary with a `*`, as `sha256sum -b` writes them.
pub fn parse_sums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (hash, name) = line.trim().split_once(char::is_whitespace)?;
            let name = name.trim_start().trim_start_matches('*');
            (hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) && !name.is_empty())
                .then(|| (hash.to_ascii_lowercase(), name.to_string()))
        })
        .collect()
}

/// Whether `bytes` are the file `name` as the sums file knows it.
pub fn matches_sums(sums: &str, name: &str, bytes: &[u8]) -> bool {
    parse_sums(sums)
        .iter()
        .any(|(hash, n)| n == name && *hash == sha256_hex(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures_are_checked_and_edits_refused() {
        let key = KeyPair::generate();
        let sums = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef  freesize-linux-x86_64.tar.gz\n";
        let signature = sign_sums(&key, sums);
        assert!(verify_sums(&key.public(), sums, &signature));
        assert!(verify_sums(&key.public(), sums, &format!("{signature}\n")));
        let mut edited = sums.to_vec();
        edited[0] = b'f';
        assert!(!verify_sums(&key.public(), &edited, &signature));
        assert!(!verify_sums(
            &KeyPair::generate().public(),
            sums,
            &signature
        ));
        // The same bytes signed as a device message don't count.
        assert!(!verify_sums(&key.public(), sums, &key.sign(sums)));
    }

    #[test]
    fn sums_files_are_read() {
        let text = "abcd  a.txt\nnot a hash  b.txt\n\
                    0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef *c.zip\n";
        assert_eq!(
            parse_sums(text),
            vec![(
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
                "c.zip".to_string()
            )]
        );
        let hello = format!("{}  hello.txt\n", sha256_hex(b"hello"));
        assert!(matches_sums(&hello, "hello.txt", b"hello"));
        assert!(!matches_sums(&hello, "hello.txt", b"hullo"));
        assert!(!matches_sums(&hello, "other.txt", b"hello"));
    }

    #[test]
    fn the_public_key_file_is_read_or_empty() {
        assert_eq!(public_key_in("# comment\n\n"), None);
        let key = KeyPair::generate().public();
        assert_eq!(
            public_key_in(&format!("# the key\n{}\n", key.to_base64())),
            Some(key)
        );
        // Whatever the repository holds parses, or is empty.
        let _ = release_key();
    }
}
