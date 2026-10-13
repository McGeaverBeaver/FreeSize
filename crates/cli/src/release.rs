//! `freesize release …`: the release key, signing a release's sums file,
//! and checking one. See release/README.md.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use clap::Subcommand;
use freesize_proto::KeyPair;
use freesize_proto::release::{public_key_in, release_key, sign_sums, verify_sums};

#[derive(Subcommand, Debug)]
pub enum ReleaseAction {
    /// Make the release key (once, on the build machine) and print its
    /// public half, to paste into release/public-key.txt.
    Key {
        /// Where to keep the private key (default: ~/.freesize/release-key).
        file: Option<PathBuf>,
    },
    /// Sign a release's SHA256SUMS.txt; writes SHA256SUMS.txt.sig beside it.
    Sign {
        sums: PathBuf,
        /// The private key from `release key`.
        #[arg(long)]
        key: PathBuf,
    },
    /// Check a sums file's signature with the key this build carries, or
    /// the one in a public-key.txt.
    Verify {
        sums: PathBuf,
        /// The signature file (default: the sums file's name plus .sig).
        #[arg(long)]
        signature: Option<PathBuf>,
        /// A public-key.txt to check with instead of the built-in key.
        #[arg(long)]
        public_key: Option<PathBuf>,
    },
}

pub fn main(action: ReleaseAction) -> Result<(), String> {
    match action {
        ReleaseAction::Key { file } => {
            let file = file.unwrap_or_else(default_key_path);
            make_key(&file)
        }
        ReleaseAction::Sign { sums, key } => sign(&sums, &key),
        ReleaseAction::Verify {
            sums,
            signature,
            public_key,
        } => verify(&sums, signature.as_deref(), public_key.as_deref()),
    }
}

fn default_key_path() -> PathBuf {
    std::env::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".freesize")
        .join("release-key")
}

fn read(path: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))
}

fn make_key(file: &Path) -> Result<(), String> {
    if file.exists() {
        return Err(format!(
            "{} exists already; move it away first if you mean to replace the key",
            file.display()
        ));
    }
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let key = KeyPair::generate();
    let text = format!("{}\n", STANDARD.encode(key.seed()));
    write_private(file, text.as_bytes())?;
    println!(
        "Release key made; its private half is in {} and must never be committed.\n\
         Public half, for release/public-key.txt:\n{}\nFingerprint {}",
        file.display(),
        key.public().to_base64(),
        key.public().fingerprint()
    );
    Ok(())
}

/// Writes the key readable by its owner only.
fn write_private(file: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(file)
            .map_err(|e| format!("{}: {e}", file.display()))?;
        out.write_all(bytes)
            .map_err(|e| format!("{}: {e}", file.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::write(file, bytes).map_err(|e| format!("{}: {e}", file.display()))
    }
}

fn load_key(file: &Path) -> Result<KeyPair, String> {
    let text = String::from_utf8_lossy(&read(file)?).trim().to_string();
    let seed: [u8; 32] = STANDARD
        .decode(&text)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| format!("{}: not a release key", file.display()))?;
    Ok(KeyPair::from_seed(seed))
}

fn signature_path(sums: &Path) -> PathBuf {
    let mut name = sums.as_os_str().to_owned();
    name.push(".sig");
    PathBuf::from(name)
}

fn sign(sums: &Path, key: &Path) -> Result<(), String> {
    let key = load_key(key)?;
    let bytes = read(sums)?;
    let signature = sign_sums(&key, &bytes);
    let out = signature_path(sums);
    std::fs::write(&out, format!("{signature}\n"))
        .map_err(|e| format!("{}: {e}", out.display()))?;
    println!(
        "Signed {} with key {}: {}",
        sums.display(),
        key.public().fingerprint(),
        out.display()
    );
    Ok(())
}

fn verify(sums: &Path, signature: Option<&Path>, public_key: Option<&Path>) -> Result<(), String> {
    let key = match public_key {
        Some(file) => public_key_in(&String::from_utf8_lossy(&read(file)?))
            .ok_or_else(|| format!("{}: no public key in it", file.display()))?,
        None => release_key().ok_or("this build carries no release key; pass --public-key")?,
    };
    let signature_file = signature.map_or_else(|| signature_path(sums), Path::to_path_buf);
    let bytes = read(sums)?;
    let signature = String::from_utf8_lossy(&read(&signature_file)?).to_string();
    if verify_sums(&key, &bytes, &signature) {
        println!(
            "{} is signed by the release key {}",
            sums.display(),
            key.fingerprint()
        );
        Ok(())
    } else {
        Err(format!(
            "{} is NOT signed by the release key {}: don't install these files",
            sums.display(),
            key.fingerprint()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_signs_and_an_edit_is_refused() {
        let dir = std::env::temp_dir().join(format!("freesize-release-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let key = dir.join("release-key");
        make_key(&key).unwrap();
        assert!(make_key(&key).is_err());
        let public = dir.join("public-key.txt");
        std::fs::write(
            &public,
            format!("# key\n{}\n", load_key(&key).unwrap().public().to_base64()),
        )
        .unwrap();
        let sums = dir.join("SHA256SUMS.txt");
        std::fs::write(&sums, "0000  a.zip\n").unwrap();
        sign(&sums, &key).unwrap();
        verify(&sums, None, Some(&public)).unwrap();
        std::fs::write(&sums, "0001  a.zip\n").unwrap();
        assert!(verify(&sums, None, Some(&public)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
