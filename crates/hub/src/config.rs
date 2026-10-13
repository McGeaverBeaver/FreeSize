//! Settings, from command-line flags or environment variables (the Docker
//! image and the Unraid template use the variables).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// FreeSize Hub: one dashboard for the storage of all your devices.
#[derive(Clone, Debug, Parser)]
#[command(
    version,
    about,
    after_help = "`freesize-hub health` checks that the Hub on this machine answers, \
                  for a container's health check."
)]
pub struct Config {
    /// Address and port to listen on. Bind it to a LAN or WireGuard address
    /// to keep the Hub off the internet.
    #[arg(long, env = "FREESIZE_HUB_LISTEN", default_value = "0.0.0.0:9090")]
    pub listen: SocketAddr,

    /// Folder for the database and the Hub's key. Back it up.
    #[arg(long, env = "FREESIZE_HUB_DATA", default_value_os_t = default_data_dir())]
    pub data: PathBuf,

    /// The address people and devices use to reach the Hub, e.g.
    /// `https://freesize.home.lan`. Used in pairing links and as the base of
    /// the sign-in redirect (`<url>/auth/callback`).
    #[arg(long, env = "FREESIZE_HUB_URL")]
    pub url: Option<String>,

    /// Name shown in the dashboard and to devices.
    #[arg(long, env = "FREESIZE_HUB_NAME", default_value = "FreeSize Hub")]
    pub name: String,

    /// OpenID Connect issuer, e.g. Authentik's
    /// `https://auth.home.lan/application/o/freesize/`.
    #[arg(long, env = "FREESIZE_OIDC_ISSUER")]
    pub oidc_issuer: Option<String>,

    #[arg(long, env = "FREESIZE_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,

    #[arg(long, env = "FREESIZE_OIDC_CLIENT_SECRET", hide_env_values = true)]
    pub oidc_client_secret: Option<String>,

    /// File holding the client secret instead (Docker secrets).
    #[arg(long, env = "FREESIZE_OIDC_CLIENT_SECRET_FILE")]
    pub oidc_client_secret_file: Option<PathBuf>,

    /// Members of this group can do everything. While it's the default,
    /// `disktree-admins` (the default before the rename) counts too.
    #[arg(long, env = "FREESIZE_ADMIN_GROUP", default_value = "freesize-admins")]
    pub admin_group: String,

    /// Members of this group can look at every device. Device groups can name
    /// other groups that see only their devices. While it's the default,
    /// `disktree-viewers` counts too.
    #[arg(
        long,
        env = "FREESIZE_VIEWER_GROUP",
        default_value = "freesize-viewers"
    )]
    pub viewer_group: String,

    /// Bearer token for `/metrics`; without it, metrics are off.
    #[arg(long, env = "FREESIZE_METRICS_TOKEN", hide_env_values = true)]
    pub metrics_token: Option<String>,

    /// HTTPS: `auto` serves it with the Hub's own certificate (or the files
    /// below) and still answers plain http on the same port; `off` is plain
    /// http only, for a reverse proxy that does TLS in front.
    #[arg(long, env = "FREESIZE_TLS", default_value = "auto", value_parser = ["auto", "off"])]
    pub tls: String,

    /// An administrator's certificate (PEM, with its chain) instead of the
    /// Hub's own. Reloaded when the file changes.
    #[arg(long, env = "FREESIZE_TLS_CERT_FILE", requires = "tls_key_file")]
    pub tls_cert_file: Option<PathBuf>,

    /// The private key (PEM) for that certificate.
    #[arg(long, env = "FREESIZE_TLS_KEY_FILE", requires = "tls_cert_file")]
    pub tls_key_file: Option<PathBuf>,
}

/// The host in an address like `https://hub.lan:9090/x` or `http://[::1]:9090`.
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split('/').next()?;
    let host = if let Some(rest) = authority.strip_prefix('[') {
        rest.split(']').next()?
    } else {
        authority
            .rsplit_once(':')
            .map_or(authority, |(host, _)| host)
    };
    (!host.is_empty()).then(|| host.to_string())
}

fn default_data_dir() -> PathBuf {
    let base = if cfg!(windows) {
        std::env::var_os("ProgramData").map(PathBuf::from)
    } else if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        Some(PathBuf::from(xdg))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share"))
    };
    base.map_or_else(
        || PathBuf::from("freesize-hub-data"),
        |b| {
            b.join(if cfg!(windows) {
                "FreeSize Hub"
            } else {
                "freesize-hub"
            })
        },
    )
}

/// Membership of `group`, or of `old` while `group` is still `default`, so
/// an Authentik set up before the rename keeps working.
fn in_group(groups: &[String], group: &str, default: &str, old: &str) -> bool {
    groups
        .iter()
        .any(|g| g == group || (group == default && g == old))
}

/// The sign-in settings, when they're all there.
#[derive(Clone, Debug)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

impl Config {
    /// When the data folder is the default and only the one from before
    /// the rename exists, moves that into place.
    pub fn adopt_old_data(&mut self) {
        if self.data == default_data_dir() {
            self.data = freesize_core::renamed::adopt_dir(self.data.clone());
        }
    }

    /// Whether these groups make someone an admin.
    pub fn is_admin(&self, groups: &[String]) -> bool {
        in_group(
            groups,
            &self.admin_group,
            "freesize-admins",
            "disktree-admins",
        )
    }

    /// Whether these groups let someone see every device.
    pub fn is_viewer(&self, groups: &[String]) -> bool {
        in_group(
            groups,
            &self.viewer_group,
            "freesize-viewers",
            "disktree-viewers",
        )
    }

    /// Whether the Hub serves HTTPS itself.
    pub fn tls_on(&self) -> bool {
        self.tls != "off"
    }

    /// The public address without a trailing slash. With the Hub serving
    /// HTTPS, it's `https://` whatever the setting says: the Hub answers
    /// plain http on the same port only for devices paired before it did.
    pub fn public_url(&self) -> Option<String> {
        let url = self
            .url
            .as_deref()
            .and_then(freesize_proto::pairing::normalize_hub)?;
        Some(match (self.tls_on(), url.strip_prefix("http://")) {
            (true, Some(rest)) => format!("https://{rest}"),
            _ => url,
        })
    }

    /// Whether the address was given as plain `http://` although the Hub
    /// serves HTTPS, so sign-in's redirect URI is `https://` now.
    pub fn url_was_http(&self) -> bool {
        self.tls_on()
            && self
                .url
                .as_deref()
                .is_some_and(|u| u.trim().starts_with("http://"))
    }

    /// The host the Hub's own certificate is for: the address's, else this
    /// machine's name.
    pub fn tls_host(&self) -> String {
        self.url
            .as_deref()
            .and_then(host_of)
            .unwrap_or_else(freesize_agent::hostname)
    }

    /// Whether the dashboard is reached over HTTPS, so cookies are marked
    /// secure.
    pub fn https(&self) -> bool {
        self.public_url().is_some_and(|u| u.starts_with("https://"))
    }

    /// The sign-in settings, or what's missing.
    pub fn oidc(&self) -> Result<OidcSettings, String> {
        let mut missing = Vec::new();
        if self.public_url().is_none() {
            missing.push("FREESIZE_HUB_URL");
        }
        if self.oidc_issuer.is_none() {
            missing.push("FREESIZE_OIDC_ISSUER");
        }
        if self.oidc_client_id.is_none() {
            missing.push("FREESIZE_OIDC_CLIENT_ID");
        }
        let secret = match (&self.oidc_client_secret, &self.oidc_client_secret_file) {
            (Some(secret), _) => Some(secret.trim().to_string()),
            (None, Some(file)) => std::fs::read_to_string(file)
                .ok()
                .map(|s| s.trim().to_string()),
            (None, None) => None,
        };
        if secret.as_deref().is_none_or(str::is_empty) {
            missing.push("FREESIZE_OIDC_CLIENT_SECRET");
        }
        if !missing.is_empty() {
            return Err(format!("not set: {}", missing.join(", ")));
        }
        Ok(OidcSettings {
            issuer: self.oidc_issuer.clone().unwrap_or_default(),
            client_id: self.oidc_client_id.clone().unwrap_or_default(),
            client_secret: secret.unwrap_or_default(),
            redirect_uri: format!("{}/auth/callback", self.public_url().unwrap_or_default()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Config {
        let mut all = vec!["freesize-hub"];
        all.extend_from_slice(args);
        Config::parse_from(all)
    }

    #[test]
    fn sign_in_needs_every_setting() {
        let config = parse(&["--url", "freesize.lan/", "--tls", "off"]);
        assert_eq!(config.public_url().as_deref(), Some("https://freesize.lan"));
        assert!(config.https());
        let missing = config.oidc().unwrap_err();
        assert!(missing.contains("FREESIZE_OIDC_ISSUER"));
        assert!(!missing.contains("FREESIZE_HUB_URL"));

        let config = parse(&[
            "--tls",
            "off",
            "--url",
            "http://10.0.0.2:9090",
            "--oidc-issuer",
            "https://auth.lan/application/o/freesize/",
            "--oidc-client-id",
            "id",
            "--oidc-client-secret",
            " secret\n",
        ]);
        let oidc = config.oidc().unwrap();
        assert_eq!(oidc.client_secret, "secret");
        assert_eq!(oidc.redirect_uri, "http://10.0.0.2:9090/auth/callback");
        assert!(!config.https());
    }

    #[test]
    fn https_by_default_turns_the_address_https() {
        let config = parse(&["--url", "http://10.0.0.2:9090"]);
        assert!(config.tls_on());
        assert_eq!(
            config.public_url().as_deref(),
            Some("https://10.0.0.2:9090")
        );
        assert!(config.https() && config.url_was_http());
        assert_eq!(config.tls_host(), "10.0.0.2");
        let config = parse(&["--url", "https://hub.lan/", "--tls", "off"]);
        assert_eq!(config.public_url().as_deref(), Some("https://hub.lan"));
        assert!(!config.url_was_http());
        assert_eq!(host_of("http://[::1]:9090/x"), Some("::1".into()));
        assert_eq!(host_of("hub.lan"), Some("hub.lan".into()));
        assert_eq!(host_of("https://hub.lan:9090"), Some("hub.lan".into()));
        assert_eq!(host_of("https://"), None);
    }

    #[test]
    fn groups_from_before_the_rename_count_while_the_default_is_used() {
        let groups = |g: &[&str]| g.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let config = parse(&[]);
        assert!(config.is_admin(&groups(&["freesize-admins"])));
        assert!(config.is_admin(&groups(&["disktree-admins"])));
        assert!(config.is_viewer(&groups(&["disktree-viewers"])));
        assert!(!config.is_admin(&groups(&["disktree-viewers"])));

        let config = parse(&["--admin-group", "nas-admins"]);
        assert!(config.is_admin(&groups(&["nas-admins"])));
        assert!(!config.is_admin(&groups(&["disktree-admins"])));
        assert!(!config.is_admin(&groups(&["freesize-admins"])));
    }
}
