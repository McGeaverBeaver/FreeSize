//! How requests reach the Hub. Desktops and servers use `ureq`; the Android
//! app plugs in Java's HTTP client. Each request says what to trust: the
//! system's certificates, or the one authority the Hub made for itself,
//! which a device trusts for its Hub's address and nothing else.

/// Which certificates a request accepts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Trust {
    /// The system's certificate store (an administrator's certificate, a
    /// reverse proxy, a home CA installed on the machine).
    #[default]
    System,
    /// Exactly this authority (PEM): the Hub's own, learned when pairing.
    Authority(String),
}

/// One HTTP request and its answer.
pub trait Transport {
    /// Sends a request; returns the status and body. `Err` only when the Hub
    /// couldn't be reached at all, a refused certificate included.
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<&[u8]>,
        timeout_secs: u64,
        trust: &Trust,
    ) -> Result<(u16, Vec<u8>), String>;

    /// Fetches a file from the internet (a release and its sums), following
    /// redirects, with the system's certificates. By default a plain GET,
    /// which can't follow one.
    fn download(
        &self,
        url: &str,
        headers: &[(&str, String)],
        timeout_secs: u64,
    ) -> Result<(u16, Vec<u8>), String> {
        self.request("GET", url, headers, None, timeout_secs, &Trust::System)
    }
}

#[cfg(feature = "ureq")]
pub use self::ureq_transport::UreqTransport;

#[cfg(feature = "ureq")]
mod ureq_transport {
    use std::io::Read;
    use std::sync::Mutex;
    use std::time::Duration;

    use ureq::tls::{Certificate, PemItem, RootCerts, TlsConfig, TlsProvider, parse_pem};

    use super::Trust;

    /// `ureq` with the system's certificate store, so a Hub behind a reverse
    /// proxy with a certificate from a home CA works once that CA is
    /// installed on the machine; or with exactly the Hub's own authority.
    pub struct UreqTransport {
        system: ureq::Agent,
        /// The system agent again, following redirects, for downloads.
        following: ureq::Agent,
        /// The agent for the last authority asked for, so polling doesn't
        /// build one per request.
        pinned: Mutex<Option<(String, ureq::Agent)>>,
    }

    impl Default for UreqTransport {
        fn default() -> Self {
            UreqTransport::new()
        }
    }

    fn agent(roots: RootCerts) -> ureq::Agent {
        agent_following(roots, 0)
    }

    fn agent_following(roots: RootCerts, redirects: u32) -> ureq::Agent {
        let provider = if cfg!(any(windows, target_os = "macos")) {
            TlsProvider::NativeTls
        } else {
            TlsProvider::Rustls
        };
        let tls = TlsConfig::builder()
            .provider(provider)
            .root_certs(roots)
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .max_redirects(redirects)
            .build()
            .into()
    }

    /// A release's biggest file is well under this.
    const MAX_DOWNLOAD: u64 = 512 * 1024 * 1024;

    impl UreqTransport {
        pub fn new() -> UreqTransport {
            UreqTransport {
                system: agent(RootCerts::PlatformVerifier),
                following: agent_following(RootCerts::PlatformVerifier, 10),
                pinned: Mutex::new(None),
            }
        }

        fn for_trust(&self, trust: &Trust) -> Result<ureq::Agent, String> {
            let Trust::Authority(pem) = trust else {
                return Ok(self.system.clone());
            };
            let mut pinned = self.pinned.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((known, agent)) = &*pinned
                && known == pem
            {
                return Ok(agent.clone());
            }
            let certs: Vec<Certificate<'static>> = parse_pem(pem.as_bytes())
                .filter_map(|item| match item {
                    Ok(PemItem::Certificate(cert)) => Some(Ok(cert)),
                    Ok(_) => None,
                    Err(err) => Some(Err(err)),
                })
                .collect::<Result<_, _>>()
                .map_err(|e| format!("the Hub's certificate authority: {e}"))?;
            if certs.is_empty() {
                return Err("the Hub's certificate authority is empty".into());
            }
            let agent = agent(RootCerts::from(certs));
            *pinned = Some((pem.clone(), agent.clone()));
            Ok(agent)
        }
    }

    impl super::Transport for UreqTransport {
        fn request(
            &self,
            method: &str,
            url: &str,
            headers: &[(&str, String)],
            body: Option<&[u8]>,
            timeout_secs: u64,
            trust: &Trust,
        ) -> Result<(u16, Vec<u8>), String> {
            let agent = self.for_trust(trust)?;
            send(
                &agent,
                method,
                url,
                headers,
                body,
                timeout_secs,
                64 * 1024 * 1024,
            )
        }

        fn download(
            &self,
            url: &str,
            headers: &[(&str, String)],
            timeout_secs: u64,
        ) -> Result<(u16, Vec<u8>), String> {
            send(
                &self.following,
                "GET",
                url,
                headers,
                None,
                timeout_secs,
                MAX_DOWNLOAD,
            )
        }
    }

    fn send(
        agent: &ureq::Agent,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<&[u8]>,
        timeout_secs: u64,
        limit: u64,
    ) -> Result<(u16, Vec<u8>), String> {
        {
            let timeout = Some(Duration::from_secs(timeout_secs));
            let mut response = match method {
                "GET" => {
                    let mut request = agent.get(url).config().timeout_global(timeout).build();
                    for (name, value) in headers {
                        request = request.header(*name, value);
                    }
                    request.call()
                }
                _ => {
                    let mut request = agent.post(url).config().timeout_global(timeout).build();
                    for (name, value) in headers {
                        request = request.header(*name, value);
                    }
                    request.send(body.unwrap_or_default())
                }
            }
            .map_err(|e| e.to_string())?;
            let status = response.status().as_u16();
            let mut bytes = Vec::new();
            response
                .body_mut()
                .with_config()
                .limit(limit)
                .reader()
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Ok((status, bytes))
        }
    }
}
