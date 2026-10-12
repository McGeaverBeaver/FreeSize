//! DiskTree Hub: a small server with a web dashboard that collects what
//! paired devices report about their storage, and passes on actions an
//! admin asks for, which each device carries out only after approving them.

pub mod api;
pub mod audit;
pub mod auth;
pub mod commands;
pub mod config;
pub mod dashboard;
pub mod db;
pub mod devices;
pub mod error;
pub mod history;
pub mod web;

use std::net::SocketAddr;
use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use disktree_proto::KeyPair;

pub use config::Config;
pub use db::Db;

/// Everything request handlers share.
#[derive(Clone)]
pub struct AppState(Arc<Shared>);

pub struct Shared {
    pub config: Config,
    pub db: Db,
    /// The Hub's own key: it signs commands and pairing answers.
    pub key: KeyPair,
    pub oidc: auth::Oidc,
    /// Wakes devices' waiting polls.
    pub wakers: devices::Wakers,
    pub pair_limiter: devices::PairLimiter,
}

impl Deref for AppState {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.0
    }
}

impl AppState {
    pub fn new(config: Config, db: Db, key: KeyPair) -> AppState {
        let oidc = auth::Oidc::new(config.oidc());
        AppState(Arc::new(Shared {
            config,
            db,
            key,
            oidc,
            wakers: devices::Wakers::default(),
            pair_limiter: devices::PairLimiter::default(),
        }))
    }

    /// Opens the data folder: the database and the Hub's key, made on first
    /// start.
    pub fn open(config: Config) -> Result<AppState, String> {
        std::fs::create_dir_all(&config.data)
            .map_err(|e| format!("{}: {e}", config.data.display()))?;
        let db = Db::open(&config.data.join("hub.db"))
            .map_err(|e| format!("{}: {e}", config.data.join("hub.db").display()))?;
        let key = load_or_create_key(&config.data.join("hub-key"))?;
        Ok(AppState::new(config, db, key))
    }
}

/// The Hub's key, kept as its base64 seed in a file only the Hub's user can
/// read.
fn load_or_create_key(path: &Path) -> Result<KeyPair, String> {
    let fail = |e: std::io::Error| format!("{}: {e}", path.display());
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let seed: [u8; 32] = STANDARD
                .decode(text.trim())
                .ok()
                .and_then(|b| b.try_into().ok())
                .ok_or_else(|| format!("{}: not a DiskTree Hub key", path.display()))?;
            Ok(KeyPair::from_seed(seed))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            let key = KeyPair::generate();
            write_private(path, &STANDARD.encode(key.seed())).map_err(fail)?;
            tracing::info!("made the Hub's key {}", key.public().fingerprint());
            Ok(key)
        }
        Err(err) => Err(fail(err)),
    }
}

/// Writes a file only its owner can read.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    options.open(path)?.write_all(text.as_bytes())
}

/// Every route of the Hub.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(web::index))
        .route("/app.js", get(web::script))
        .route("/app.css", get(web::style))
        .route("/icon.svg", get(web::icon))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/api/status", get(api::status))
        .route("/api/me", get(api::me))
        .route("/api/audit", get(api::audit_log))
        .route("/api/audit/verify", get(api::audit_verify))
        .route("/api/devices", get(dashboard::list))
        .route(
            "/api/devices/{id}",
            get(dashboard::detail)
                .patch(dashboard::update)
                .delete(dashboard::remove),
        )
        .route("/api/devices/{id}/audit", get(dashboard::device_audit))
        .route("/api/scans/{id}/folders", get(dashboard::folder))
        .route("/api/scans/{id}/files", get(dashboard::files))
        .route(
            "/api/groups",
            get(dashboard::list_groups).post(dashboard::create_group),
        )
        .route(
            "/api/groups/{id}",
            axum::routing::patch(dashboard::update_group).delete(dashboard::delete_group),
        )
        .route("/api/pairing-codes", post(dashboard::pairing_code))
        .route("/api/devices/{id}/commands", post(commands::create))
        .route("/api/commands", get(commands::list))
        .route("/api/commands/{id}/cancel", post(commands::cancel))
        // Devices.
        .route("/api/v1/hello", get(devices::hello))
        .route("/api/v1/pair", post(devices::pair))
        .route(
            "/api/v1/report",
            // A whole drive's scan can be a few MB.
            post(devices::report).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route("/api/v1/poll", post(devices::poll))
        .route("/api/v1/devices/{fingerprint}", get(devices::device_key))
        .route("/api/v1/commands/{id}", post(commands::update))
        .route("/api/v1/decisions", post(commands::decide))
        .layer(axum::middleware::from_fn(web::security_headers))
        .with_state(state)
}

/// Runs the Hub until Ctrl+C or SIGTERM.
pub async fn run(config: Config) -> Result<(), String> {
    let listen = config.listen;
    let state = AppState::open(config)?;
    if let Err(missing) = state.oidc.settings() {
        tracing::warn!("sign-in isn't set up ({missing}); the dashboard can't be used yet");
    }
    tracing::info!(
        "DiskTree Hub {} listening on {listen}, key {}",
        env!("CARGO_PKG_VERSION"),
        state.key.public().fingerprint()
    );
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("{listen}: {e}"))?;
    serve(listener, state).await
}

/// Serves on an open listener (tests use a random port).
pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> Result<(), String> {
    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await
    .map_err(|e| e.to_string())
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            signal.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = term => {}
    }
}
