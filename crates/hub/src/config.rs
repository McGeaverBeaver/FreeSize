//! Settings, from command-line flags or environment variables (the Docker
//! image and the Unraid template use the variables).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::Parser;

/// DiskTree Hub: one dashboard for the storage of all your devices.
#[derive(Clone, Debug, Parser)]
#[command(version, about)]
pub struct Config {
    /// Address and port to listen on. Bind it to a LAN or WireGuard address
    /// to keep the Hub off the internet.
    #[arg(long, env = "DISKTREE_HUB_LISTEN", default_value = "0.0.0.0:9090")]
    pub listen: SocketAddr,

    /// Folder for the database and the Hub's key. Back it up.
    #[arg(long, env = "DISKTREE_HUB_DATA", default_value_os_t = default_data_dir())]
    pub data: PathBuf,

    /// The address people and devices use to reach the Hub, e.g.
    /// `https://disktree.home.lan`. Used in pairing links and as the base of
    /// the sign-in redirect (`<url>/auth/callback`).
    #[arg(long, env = "DISKTREE_HUB_URL")]
    pub url: Option<String>,

    /// Name shown in the dashboard and to devices.
    #[arg(long, env = "DISKTREE_HUB_NAME", default_value = "DiskTree Hub")]
    pub name: String,

    /// OpenID Connect issuer, e.g. Authentik's
    /// `https://auth.home.lan/application/o/disktree/`.
    #[arg(long, env = "DISKTREE_OIDC_ISSUER")]
    pub oidc_issuer: Option<String>,

    #[arg(long, env = "DISKTREE_OIDC_CLIENT_ID")]
    pub oidc_client_id: Option<String>,

    #[arg(long, env = "DISKTREE_OIDC_CLIENT_SECRET", hide_env_values = true)]
    pub oidc_client_secret: Option<String>,

    /// File holding the client secret instead (Docker secrets).
    #[arg(long, env = "DISKTREE_OIDC_CLIENT_SECRET_FILE")]
    pub oidc_client_secret_file: Option<PathBuf>,

    /// Members of this group can do everything.
    #[arg(long, env = "DISKTREE_ADMIN_GROUP", default_value = "disktree-admins")]
    pub admin_group: String,

    /// Members of this group can look at every device. Device groups can name
    /// other groups that see only their devices.
    #[arg(
        long,
        env = "DISKTREE_VIEWER_GROUP",
        default_value = "disktree-viewers"
    )]
    pub viewer_group: String,

    /// Bearer token for `/metrics`; without it, metrics are off.
    #[arg(long, env = "DISKTREE_METRICS_TOKEN", hide_env_values = true)]
    pub metrics_token: Option<String>,
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
        || PathBuf::from("disktree-hub-data"),
        |b| {
            b.join(if cfg!(windows) {
                "DiskTree Hub"
            } else {
                "disktree-hub"
            })
        },
    )
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
    /// The public address without a trailing slash.
    pub fn public_url(&self) -> Option<String> {
        self.url
            .as_deref()
            .and_then(disktree_proto::pairing::normalize_hub)
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
            missing.push("DISKTREE_HUB_URL");
        }
        if self.oidc_issuer.is_none() {
            missing.push("DISKTREE_OIDC_ISSUER");
        }
        if self.oidc_client_id.is_none() {
            missing.push("DISKTREE_OIDC_CLIENT_ID");
        }
        let secret = match (&self.oidc_client_secret, &self.oidc_client_secret_file) {
            (Some(secret), _) => Some(secret.trim().to_string()),
            (None, Some(file)) => std::fs::read_to_string(file)
                .ok()
                .map(|s| s.trim().to_string()),
            (None, None) => None,
        };
        if secret.as_deref().is_none_or(str::is_empty) {
            missing.push("DISKTREE_OIDC_CLIENT_SECRET");
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
        let mut all = vec!["disktree-hub"];
        all.extend_from_slice(args);
        Config::parse_from(all)
    }

    #[test]
    fn sign_in_needs_every_setting() {
        let config = parse(&["--url", "disktree.lan/"]);
        assert_eq!(config.public_url().as_deref(), Some("https://disktree.lan"));
        assert!(config.https());
        let missing = config.oidc().unwrap_err();
        assert!(missing.contains("DISKTREE_OIDC_ISSUER"));
        assert!(!missing.contains("DISKTREE_HUB_URL"));

        let config = parse(&[
            "--url",
            "http://10.0.0.2:9090",
            "--oidc-issuer",
            "https://auth.lan/application/o/disktree/",
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
}
