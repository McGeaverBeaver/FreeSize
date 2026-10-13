//! HTTPS for the Hub. By default the Hub makes its own small certificate
//! authority on first start (kept in the data folder next to its key) and a
//! certificate for its address, signed by it: SHA-256, the address in the
//! subject alternative names, server authentication, 825 days at most
//! (Apple's limit), renewed a month before it ends or when the address
//! changes. Devices learn the authority when they pair and trust it for this
//! Hub's address only. An administrator's certificate (`--tls-cert-file`
//! and `--tls-key-file`) replaces it and is reloaded when the files change.
//! `--tls off` keeps plain http for a reverse proxy that does TLS in front.
//!
//! Plain http is still answered on the same port: for the container's
//! health check, and for devices paired before the Hub served HTTPS, which
//! are told the new address in a signed answer and move over.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::SystemTime;

use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
};
use rustls::ServerConfig;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::Serialize;
use time::{Duration, OffsetDateTime};
use tokio_rustls::TlsAcceptor;
use x509_parser::prelude::{FromDer, GeneralName, X509Certificate};

use crate::Config;

/// The Hub's own authority and its key, in the data folder.
pub const CA_FILE: &str = "tls-ca.pem";
const CA_KEY_FILE: &str = "tls-ca-key.pem";
/// The certificate for the Hub's address, signed by that authority.
pub const CERT_FILE: &str = "tls-cert.pem";
const KEY_FILE: &str = "tls-key.pem";

/// Longest a server certificate may be valid for Apple's devices to trust
/// it: 825 days.
pub const MAX_VALIDITY: Duration = Duration::days(825);
/// How long before it ends the Hub's own certificate is renewed.
const RENEW_BEFORE: Duration = Duration::days(30);
/// The authority lasts long; devices keep it for as long as they're paired.
const AUTHORITY_VALIDITY: Duration = Duration::days(20 * 365);
/// Certificates start a little in the past, for clocks that are behind.
const SKEW: Duration = Duration::days(1);

/// Where the certificate comes from.
enum Source {
    /// Made by the Hub, for these names, in this folder.
    Own { dir: PathBuf, names: Vec<String> },
    /// An administrator's files.
    Files { cert: PathBuf, key: PathBuf },
}

/// What the dashboard shows about the certificate.
#[derive(Clone, Debug, Serialize)]
pub struct TlsInfo {
    /// Made by the Hub itself (true), or an administrator's.
    pub own: bool,
    /// The names it's for.
    pub names: Vec<String>,
    /// When it ends, seconds since 1970.
    pub not_after: i64,
}

struct Loaded {
    info: TlsInfo,
    /// The files' modification times, to notice an administrator's new
    /// certificate.
    modified: Option<(SystemTime, SystemTime)>,
}

pub struct Tls {
    config: RwLock<Arc<ServerConfig>>,
    source: Source,
    /// The Hub's own authority (PEM), to hand to devices and browsers.
    ca: Option<String>,
    loaded: Mutex<Loaded>,
}

impl Tls {
    /// Sets HTTPS up as the settings say; `None` with `--tls off`.
    pub fn setup(config: &Config) -> Result<Option<Tls>, String> {
        if !config.tls_on() {
            return Ok(None);
        }
        let (source, ca) = match (&config.tls_cert_file, &config.tls_key_file) {
            (Some(cert), Some(key)) => (
                Source::Files {
                    cert: cert.clone(),
                    key: key.clone(),
                },
                None,
            ),
            _ => {
                let dir = config.data.clone();
                let names = vec![config.tls_host()];
                let ca = own_authority(&dir)?;
                issue_if_needed(&dir, &ca, &names, OffsetDateTime::now_utc())?;
                (Source::Own { dir, names }, Some(ca.0))
            }
        };
        let (server, loaded) = load(&source)?;
        Ok(Some(Tls {
            config: RwLock::new(server),
            source,
            ca,
            loaded: Mutex::new(loaded),
        }))
    }

    /// Accepts one connection with the current certificate.
    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(
            self.config
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        )
    }

    /// The Hub's own authority (PEM), when it has one.
    pub fn ca_pem(&self) -> Option<&str> {
        self.ca.as_deref()
    }

    pub fn info(&self) -> TlsInfo {
        self.loaded
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .info
            .clone()
    }

    /// Picks up an administrator's new certificate files, or renews the
    /// Hub's own a month before it ends. `Ok(true)` when it changed.
    pub fn refresh(&self) -> Result<bool, String> {
        let changed = match &self.source {
            Source::Files { cert, key } => {
                let now = (modified(cert)?, modified(key)?);
                self.loaded
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .modified
                    .is_none_or(|was| was != now)
            }
            Source::Own { dir, names } => {
                let ca = own_authority(dir)?;
                issue_if_needed(dir, &ca, names, OffsetDateTime::now_utc())?
            }
        };
        if !changed {
            return Ok(false);
        }
        let (server, loaded) = load(&self.source)?;
        *self.config.write().unwrap_or_else(|e| e.into_inner()) = server;
        *self.loaded.lock().unwrap_or_else(|e| e.into_inner()) = loaded;
        Ok(true)
    }
}

fn modified(path: &Path) -> Result<SystemTime, String> {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Reads the certificate and key the source names into a server
/// configuration.
fn load(source: &Source) -> Result<(Arc<ServerConfig>, Loaded), String> {
    let (cert_path, key_path, own) = match source {
        Source::Own { dir, .. } => (dir.join(CERT_FILE), dir.join(KEY_FILE), true),
        Source::Files { cert, key } => (cert.clone(), key.clone(), false),
    };
    let read = |path: &Path| std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()));
    let cert_pem = read(&cert_path)?;
    let key_pem = read(&key_path)?;
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(&cert_pem)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("{}: {e}", cert_path.display()))?;
    let leaf = certs
        .first()
        .ok_or_else(|| format!("{}: no certificate in it", cert_path.display()))?;
    let (names, not_after) = names_and_end(leaf)?;
    let key = PrivateKeyDer::from_pem_slice(&key_pem)
        .map_err(|e| format!("{}: {e}", key_path.display()))?;
    let server = server_config(certs, key)?;
    let modified = match source {
        Source::Files { cert, key } => Some((modified(cert)?, modified(key)?)),
        Source::Own { .. } => None,
    };
    Ok((
        server,
        Loaded {
            info: TlsInfo {
                own,
                names,
                not_after,
            },
            modified,
        },
    ))
}

fn server_config(
    certs: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<ServerConfig>, String> {
    let mut config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| format!("the certificate and key don't go together: {e}"))?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// The names a certificate is for, and when it ends.
fn names_and_end(der: &CertificateDer<'_>) -> Result<(Vec<String>, i64), String> {
    let (_, cert) = X509Certificate::from_der(der).map_err(|e| format!("the certificate: {e}"))?;
    let mut names = Vec::new();
    if let Ok(Some(san)) = cert.subject_alternative_name() {
        for name in &san.value.general_names {
            match name {
                GeneralName::DNSName(dns) => names.push(dns.to_string()),
                GeneralName::IPAddress(bytes) => match bytes.len() {
                    4 => names.push(
                        std::net::Ipv4Addr::from(<[u8; 4]>::try_from(*bytes).unwrap_or_default())
                            .to_string(),
                    ),
                    16 => names.push(
                        std::net::Ipv6Addr::from(<[u8; 16]>::try_from(*bytes).unwrap_or_default())
                            .to_string(),
                    ),
                    _ => {}
                },
                _ => {}
            }
        }
    }
    Ok((names, cert.validity().not_after.timestamp()))
}

/// The Hub's own authority: read from the data folder, or made and kept
/// there. Returns its certificate (PEM) and key.
fn own_authority(dir: &Path) -> Result<(String, KeyPair), String> {
    let cert_path = dir.join(CA_FILE);
    let key_path = dir.join(CA_KEY_FILE);
    match (
        std::fs::read_to_string(&cert_path),
        std::fs::read_to_string(&key_path),
    ) {
        (Ok(cert), Ok(key)) => {
            let key =
                KeyPair::from_pem(&key).map_err(|e| format!("{}: {e}", key_path.display()))?;
            Ok((cert, key))
        }
        (Err(e), _) | (_, Err(e)) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", dir.join("tls-ca*.pem").display()))
        }
        _ => {
            let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
            let now = OffsetDateTime::now_utc();
            let mut params = CertificateParams::default();
            params.distinguished_name = DistinguishedName::new();
            params
                .distinguished_name
                .push(DnType::CommonName, "FreeSize Hub authority");
            params.not_before = now - SKEW;
            params.not_after = now + AUTHORITY_VALIDITY;
            params.is_ca = IsCa::Ca(BasicConstraints::Constrained(0));
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
            let cert = params.self_signed(&key).map_err(|e| e.to_string())?.pem();
            crate::write_private(&key_path, &key.serialize_pem())
                .map_err(|e| format!("{}: {e}", key_path.display()))?;
            std::fs::write(&cert_path, &cert)
                .map_err(|e| format!("{}: {e}", cert_path.display()))?;
            tracing::info!(
                "made the Hub's certificate authority, {}",
                cert_path.display()
            );
            Ok((cert, key))
        }
    }
}

/// Makes the Hub's certificate for `names` when there is none, it's for other
/// names, or it ends within a month. `Ok(true)` when a new one was written.
fn issue_if_needed(
    dir: &Path,
    ca: &(String, KeyPair),
    names: &[String],
    now: OffsetDateTime,
) -> Result<bool, String> {
    let cert_path = dir.join(CERT_FILE);
    if let Ok(pem) = std::fs::read(&cert_path)
        && let Some(Ok(der)) = CertificateDer::pem_slice_iter(&pem).next()
        && let Ok((have, not_after)) = names_and_end(&der)
    {
        let mut have = have;
        let mut want = names.to_vec();
        have.sort();
        want.sort();
        let ends = OffsetDateTime::from_unix_timestamp(not_after).unwrap_or(now);
        if have == want && ends - now > RENEW_BEFORE && dir.join(KEY_FILE).is_file() {
            return Ok(false);
        }
    }
    let (cert, key) = issue(ca, names, now)?;
    let key_path = dir.join(KEY_FILE);
    let _ = std::fs::remove_file(&key_path);
    crate::write_private(&key_path, &key).map_err(|e| format!("{}: {e}", key_path.display()))?;
    std::fs::write(&cert_path, &cert).map_err(|e| format!("{}: {e}", cert_path.display()))?;
    tracing::info!(
        "made the Hub's certificate for {}, {}",
        names.join(", "),
        cert_path.display()
    );
    Ok(true)
}

/// A server certificate for `names` signed by the authority, as PEM, with
/// its key.
fn issue(
    ca: &(String, KeyPair),
    names: &[String],
    now: OffsetDateTime,
) -> Result<(String, String), String> {
    let issuer = Issuer::from_ca_cert_pem(&ca.0, &ca.1)
        .map_err(|e| format!("the Hub's certificate authority: {e}"))?;
    let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).map_err(|e| e.to_string())?;
    let mut params = CertificateParams::new(names.to_vec()).map_err(|e| e.to_string())?;
    params.distinguished_name = DistinguishedName::new();
    params.distinguished_name.push(
        DnType::CommonName,
        names.first().map_or("FreeSize Hub", String::as_str),
    );
    params.not_before = now - SKEW;
    params.not_after = params.not_before + MAX_VALIDITY;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    params.use_authority_key_identifier_extension = true;
    let cert = params.signed_by(&key, &issuer).map_err(|e| e.to_string())?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// Looks now and then for an administrator's new certificate files, and
/// renews the Hub's own in time.
pub async fn refresh_loop(state: crate::AppState) {
    let Some(tls) = state.tls.clone() else {
        return;
    };
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        match tls.refresh() {
            Ok(true) => tracing::info!("the certificate changed; using the new one"),
            Ok(false) => {}
            Err(err) => tracing::warn!("couldn't reload the certificate: {err}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("freesize-tls-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// What Apple's devices require of a server certificate.
    #[test]
    fn the_hubs_certificate_meets_apples_rules() {
        let dir = temp("apple");
        let ca = own_authority(&dir).unwrap();
        let now = OffsetDateTime::now_utc();
        let names = ["hub.lan".to_string(), "192.168.1.5".to_string()];
        assert!(issue_if_needed(&dir, &ca, &names, now).unwrap());
        let pem = std::fs::read(dir.join(CERT_FILE)).unwrap();
        let der = CertificateDer::pem_slice_iter(&pem)
            .next()
            .unwrap()
            .unwrap();
        let (_, cert) = X509Certificate::from_der(&der).unwrap();

        // At most 825 days, SHA-256 with an ECDSA P-256 key, serverAuth, the
        // names in the subject alternative names, and not an authority.
        let validity =
            cert.validity().not_after.timestamp() - cert.validity().not_before.timestamp();
        assert!(validity <= 825 * 86400, "{validity}");
        assert_eq!(
            cert.signature_algorithm.algorithm,
            x509_parser::oid_registry::OID_SIG_ECDSA_WITH_SHA256
        );
        assert!(matches!(
            cert.public_key().parsed().unwrap(),
            x509_parser::public_key::PublicKey::EC(key) if key.key_size() == 256
        ));
        let eku = cert.extended_key_usage().unwrap().unwrap();
        assert!(eku.value.server_auth && !eku.value.any);
        let (found, _) = names_and_end(&der).unwrap();
        assert_eq!(found, names);
        assert!(!cert.is_ca());
        let ca_der = CertificateDer::pem_slice_iter(ca.0.as_bytes())
            .next()
            .unwrap()
            .unwrap();
        let (_, ca_cert) = X509Certificate::from_der(&ca_der).unwrap();
        assert!(ca_cert.is_ca());
        ca_cert.verify_signature(None).unwrap();
        cert.verify_signature(Some(ca_cert.public_key())).unwrap();

        // Kept while it's good; renewed for other names or near its end.
        assert!(!issue_if_needed(&dir, &ca, &names, now).unwrap());
        assert!(issue_if_needed(&dir, &ca, &["hub.lan".to_string()], now).unwrap());
        assert!(!issue_if_needed(&dir, &ca, &["hub.lan".to_string()], now).unwrap());
        assert!(
            issue_if_needed(
                &dir,
                &ca,
                &["hub.lan".to_string()],
                now + Duration::days(800)
            )
            .unwrap()
        );
        // The authority stays the same across renewals.
        assert_eq!(own_authority(&dir).unwrap().0, ca.0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_certificate_and_key_load_into_a_server() {
        let dir = temp("load");
        let ca = own_authority(&dir).unwrap();
        let names = ["127.0.0.1".to_string()];
        issue_if_needed(&dir, &ca, &names, OffsetDateTime::now_utc()).unwrap();
        let source = Source::Own {
            dir: dir.clone(),
            names: names.to_vec(),
        };
        let (_, loaded) = load(&source).unwrap();
        assert!(loaded.info.own);
        assert_eq!(loaded.info.names, names);
        let files = Source::Files {
            cert: dir.join(CERT_FILE),
            key: dir.join(KEY_FILE),
        };
        let (_, loaded) = load(&files).unwrap();
        assert!(!loaded.info.own && loaded.modified.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
