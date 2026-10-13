//! The dashboard's JSON API. Everything needs a session; changes need the
//! CSRF header (see `auth`), and most changes need an admin.

use axum::Json;
use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::{Admin, User};
use crate::error::ApiResult;
use crate::{AppState, audit};

/// `GET /api/status`, without signing in: enough for the sign-in screen.
pub async fn status(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "name": state.config.name,
        "version": env!("CARGO_PKG_VERSION"),
        "sign_in": state.oidc.settings().is_ok(),
        "missing": state.oidc.settings().err(),
        "https": state.tls.is_some(),
    }))
}

#[derive(Serialize)]
pub struct Me {
    pub subject: String,
    pub name: String,
    pub email: String,
    pub admin: bool,
    pub csrf: String,
    pub hub: HubInfo,
}

#[derive(Serialize)]
pub struct HubInfo {
    pub name: String,
    pub version: &'static str,
    pub url: Option<String>,
    pub fingerprint: String,
    /// The certificate; none with TLS off.
    pub tls: Option<crate::tls::TlsInfo>,
}

/// `GET /api/me`: who is signed in, and about the Hub.
pub async fn me(State(state): State<AppState>, user: User) -> Json<Me> {
    Json(Me {
        subject: user.subject,
        name: user.name,
        email: user.email,
        admin: user.admin,
        csrf: user.csrf,
        hub: HubInfo {
            name: state.config.name.clone(),
            version: env!("CARGO_PKG_VERSION"),
            url: state.config.public_url(),
            fingerprint: state.key.public().fingerprint(),
            tls: state.tls.as_ref().map(|t| t.info()),
        },
    })
}

#[derive(Deserialize)]
pub struct AuditQuery {
    before: Option<i64>,
    device: Option<String>,
}

/// `GET /api/audit`: the log, newest first, 100 at a time.
pub async fn audit_log(
    State(state): State<AppState>,
    Admin(_): Admin,
    Query(query): Query<AuditQuery>,
) -> ApiResult<Json<Vec<audit::Entry>>> {
    let entries = state
        .db
        .call(move |c| audit::list(c, query.before, query.device.as_deref(), 100))
        .await?;
    Ok(Json(entries))
}

/// `GET /api/audit/verify`: checks the whole chain.
pub async fn audit_verify(
    State(state): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<audit::Check>> {
    Ok(Json(state.db.call(|c| audit::verify(c)).await?))
}
