//! How requests reach the Hub. Desktops and servers use `ureq`; the Android
//! app plugs in Java's HTTP client, which uses Android's own certificates.

/// One HTTP request and its answer.
pub trait Transport {
    /// Sends a request; returns the status and body. `Err` only when the Hub
    /// couldn't be reached at all.
    fn request(
        &self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<&[u8]>,
        timeout_secs: u64,
    ) -> Result<(u16, Vec<u8>), String>;
}

#[cfg(feature = "ureq")]
pub use self::ureq_transport::UreqTransport;

#[cfg(feature = "ureq")]
mod ureq_transport {
    use std::io::Read;
    use std::time::Duration;

    use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

    /// `ureq` with the system's certificate store, so a Hub behind a reverse
    /// proxy with a certificate from a home CA works once that CA is
    /// installed on the machine.
    #[derive(Clone)]
    pub struct UreqTransport {
        agent: ureq::Agent,
    }

    impl Default for UreqTransport {
        fn default() -> Self {
            UreqTransport::new()
        }
    }

    impl UreqTransport {
        pub fn new() -> UreqTransport {
            let provider = if cfg!(any(windows, target_os = "macos")) {
                TlsProvider::NativeTls
            } else {
                TlsProvider::Rustls
            };
            let tls = TlsConfig::builder()
                .provider(provider)
                .root_certs(RootCerts::PlatformVerifier)
                .build();
            let agent = ureq::Agent::config_builder()
                .tls_config(tls)
                .http_status_as_error(false)
                .max_redirects(0)
                .build()
                .into();
            UreqTransport { agent }
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
        ) -> Result<(u16, Vec<u8>), String> {
            let timeout = Some(Duration::from_secs(timeout_secs));
            let mut response = match method {
                "GET" => {
                    let mut request = self.agent.get(url).config().timeout_global(timeout).build();
                    for (name, value) in headers {
                        request = request.header(*name, value);
                    }
                    request.call()
                }
                _ => {
                    let mut request = self
                        .agent
                        .post(url)
                        .config()
                        .timeout_global(timeout)
                        .build();
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
                .limit(64 * 1024 * 1024)
                .reader()
                .read_to_end(&mut bytes)
                .map_err(|e| e.to_string())?;
            Ok((status, bytes))
        }
    }
}
