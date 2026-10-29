//! FreeSize Hub: a small server with a web dashboard that collects what
//! paired devices report about their storage, and passes on actions an
//! admin asks for, which each device carries out only after approving them.

pub mod alerts;
pub mod api;
pub mod audit;
pub mod auth;
pub mod cloud;
pub mod commands;
pub mod config;
pub mod copies;
pub mod dashboard;
pub mod db;
pub mod devices;
pub mod error;
pub mod events;
pub mod history;
pub mod limits;
pub mod maintenance;
pub mod metrics;
pub mod nextcloud;
pub mod reports;
pub mod tls;
pub mod updates;
pub mod web;

use std::net::SocketAddr;
use std::ops::Deref;
use std::path::Path;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::routing::{delete, get, post};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use freesize_proto::KeyPair;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::{GracefulShutdown, Watcher};
use hyper_util::service::TowerToHyperService;
use tokio::net::TcpStream;
use tower::ServiceExt;

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
    /// Wrong pairing codes and sign-in attempts, per address and in all.
    pub pair_limiter: limits::Limiter,
    pub login_limiter: limits::Limiter,
    /// HTTPS, unless `--tls off`.
    pub tls: Option<Arc<tls::Tls>>,
    /// The newest release, when the update check is on and found one.
    pub latest: updates::Latest,
    /// Nextcloud sign-ins under way, and listings running.
    pub nextcloud: nextcloud::Logins,
}

impl Deref for AppState {
    type Target = Shared;

    fn deref(&self) -> &Shared {
        &self.0
    }
}

impl AppState {
    pub fn new(config: Config, db: Db, key: KeyPair, tls: Option<tls::Tls>) -> AppState {
        let oidc = auth::Oidc::new(config.oidc());
        AppState(Arc::new(Shared {
            config,
            db,
            key,
            oidc,
            wakers: devices::Wakers::default(),
            pair_limiter: limits::Limiter::pairing(),
            login_limiter: limits::Limiter::sign_in(),
            latest: updates::Latest::default(),
            nextcloud: nextcloud::Logins::default(),
            tls: tls.map(Arc::new),
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
        let tls = tls::Tls::setup(&config)?;
        Ok(AppState::new(config, db, key, tls))
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
                .ok_or_else(|| format!("{}: not a FreeSize Hub key", path.display()))?;
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
        .route("/ca.pem", get(web::authority))
        .route("/auth/login", get(auth::login))
        .route("/auth/callback", get(auth::callback))
        .route("/auth/logout", post(auth::logout))
        .route("/api/status", get(api::status))
        .route("/api/me", get(api::me))
        .route("/api/audit", get(api::audit_log))
        .route("/api/audit/verify", get(api::audit_verify))
        .route(
            "/api/devices",
            get(dashboard::list).post(reports::create_device),
        )
        .route("/api/devices/export", get(reports::export_devices))
        .route(
            "/api/devices/{id}",
            get(dashboard::detail)
                .patch(dashboard::update)
                .delete(dashboard::remove),
        )
        .route("/api/devices/{id}/audit", get(dashboard::device_audit))
        .route("/api/devices/{id}/events", get(events::for_device))
        .route("/api/devices/{id}/report", get(reports::device_report))
        .route(
            "/api/devices/{id}/scans",
            post(reports::upload_scan).layer(DefaultBodyLimit::max(reports::MAX_UPLOAD)),
        )
        .route("/api/scans/{id}/folders", get(dashboard::folder))
        .route("/api/scans/{id}/files", get(dashboard::files))
        .route("/api/scans/{id}/compare", get(reports::compare))
        .route("/api/scans/{id}/export", get(reports::export_scan))
        .route("/api/search", get(reports::search))
        .route("/api/alerts", get(alerts::list))
        .route(
            "/api/alerts/over",
            axum::routing::delete(alerts::clear_over),
        )
        .route(
            "/api/alerts/{id}/hush",
            post(alerts::hush_alert).delete(alerts::unhush_alert),
        )
        .route(
            "/api/alerts/rules",
            get(alerts::get_rules).put(alerts::set_rules),
        )
        .route(
            "/api/alerts/channels",
            get(alerts::list_channels).post(alerts::add_channel),
        )
        .route(
            "/api/alerts/channels/{id}",
            axum::routing::patch(alerts::update_channel).delete(alerts::remove_channel),
        )
        .route("/api/alerts/channels/{id}/test", post(alerts::test_channel))
        .route("/api/nextcloud", get(nextcloud::list))
        .route("/api/nextcloud/connect", post(nextcloud::connect))
        .route(
            "/api/nextcloud/connect/{flow}",
            get(nextcloud::connect_status),
        )
        .route("/api/nextcloud/{id}", delete(nextcloud::remove))
        .route("/api/nextcloud/{id}/sync", post(nextcloud::sync_now))
        .route("/api/copies", get(copies::copies))
        .route(
            "/api/copies/ignored",
            post(copies::ignore).delete(copies::unignore),
        )
        .route("/metrics", get(metrics::prometheus))
        .route("/metrics/json", get(metrics::summary))
        .route(
            "/api/searches",
            get(reports::saved_searches).post(reports::save_search),
        )
        .route(
            "/api/searches/{id}",
            axum::routing::delete(reports::delete_search),
        )
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
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            web::https_only,
        ))
        .with_state(state)
}

/// Runs the Hub until Ctrl+C or SIGTERM.
pub async fn run(mut config: Config) -> Result<(), String> {
    config.adopt_old_data();
    let listen = config.listen;
    let state = AppState::open(config)?;
    if let Err(missing) = state.oidc.settings() {
        tracing::warn!("sign-in isn't set up ({missing}); the dashboard can't be used yet");
    }
    match &state.tls {
        Some(tls) => {
            let info = tls.info();
            tracing::info!(
                "serving HTTPS for {} with {} certificate",
                info.names.join(", "),
                if info.own {
                    "the Hub's own"
                } else {
                    "the administrator's"
                }
            );
            if state.config.url_was_http() {
                tracing::warn!(
                    "the address was given as http://, but the Hub serves HTTPS: pairing links \
                     and the sign-in redirect URI are {} now; set that redirect URI in Authentik",
                    state.config.public_url().unwrap_or_default()
                );
            }
        }
        None => tracing::info!("TLS is off: plain http only, for a reverse proxy in front"),
    }
    tracing::info!(
        "FreeSize Hub {} listening on {listen}, key {}",
        env!("CARGO_PKG_VERSION"),
        state.key.public().fingerprint()
    );
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(|e| format!("{listen}: {e}"))?;
    serve(listener, state).await
}

/// How a request came in, for handlers that care (plain http to a Hub that
/// serves HTTPS is redirected, and devices on it are told to move).
#[derive(Clone, Copy, Debug)]
pub struct Via {
    pub tls: bool,
    pub peer: SocketAddr,
}

/// Serves on an open listener (tests use a random port): HTTPS and plain
/// http on the same port, told apart by the first byte, until Ctrl+C or
/// SIGTERM.
pub async fn serve(listener: tokio::net::TcpListener, state: AppState) -> Result<(), String> {
    let checking = tokio::spawn(alerts::run(state.clone()));
    let refreshing = tokio::spawn(tls::refresh_loop(state.clone()));
    let housekeeping = tokio::spawn(maintenance::run(state.clone()));
    let updates = tokio::spawn(updates::run(state.clone()));
    let listing = tokio::spawn(nextcloud::run(state.clone()));
    let app = router(state.clone());
    let builder = auto::Builder::new(TokioExecutor::new());
    let graceful = GracefulShutdown::new();
    let mut shutdown = pin!(shutdown());
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(err) => {
                        tracing::warn!("accept: {err}");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let app = app.clone();
                let tls = state.tls.clone();
                let builder = builder.clone();
                let watcher = graceful.watcher();
                tokio::spawn(connection(stream, peer, tls, app, builder, watcher));
            }
            () = &mut shutdown => break,
        }
    }
    tokio::select! {
        () = graceful.shutdown() => {}
        () = tokio::time::sleep(Duration::from_secs(10)) => {}
    }
    checking.abort();
    refreshing.abort();
    housekeeping.abort();
    updates.abort();
    listing.abort();
    Ok(())
}

/// One connection: a TLS handshake starts with 0x16, anything else is
/// plain http.
async fn connection(
    stream: TcpStream,
    peer: SocketAddr,
    tls: Option<Arc<tls::Tls>>,
    app: Router,
    builder: auto::Builder<TokioExecutor>,
    watcher: Watcher,
) {
    let mut first = [0u8; 1];
    let peeked = tokio::time::timeout(Duration::from_secs(10), stream.peek(&mut first)).await;
    let is_tls = matches!(peeked, Ok(Ok(1))) && first[0] == 0x16;
    let via = Via { tls: is_tls, peer };
    let service = TowerToHyperService::new(app.map_request(
        move |mut request: axum::extract::Request<hyper::body::Incoming>| {
            request.extensions_mut().insert(via);
            request
        },
    ));
    let served = match (is_tls, tls) {
        (true, Some(tls)) => match tls.acceptor().accept(stream).await {
            Ok(stream) => {
                watcher
                    .watch(
                        builder
                            .serve_connection_with_upgrades(TokioIo::new(stream), service)
                            .into_owned(),
                    )
                    .await
            }
            Err(err) => {
                tracing::debug!("{peer}: TLS handshake: {err}");
                return;
            }
        },
        // TLS to a Hub without it: nothing sensible to answer.
        (true, None) => {
            tracing::debug!("{peer}: TLS to a plain-http Hub");
            return;
        }
        (false, _) => {
            watcher
                .watch(
                    builder
                        .serve_connection_with_upgrades(TokioIo::new(stream), service)
                        .into_owned(),
                )
                .await
        }
    };
    if let Err(err) = served {
        tracing::debug!("{peer}: {err}");
    }
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
