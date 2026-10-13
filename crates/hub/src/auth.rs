//! Signing in to the dashboard with OpenID Connect (Authentik, or any other
//! provider), and the session that follows.
//!
//! - Authorization code flow with PKCE, a `state` per sign-in and a `nonce`
//!   checked in the ID token.
//! - The ID token's signature is checked against the provider's published
//!   keys (or the client secret, for providers set to HS256), along with its
//!   issuer, audience and expiry.
//! - Roles come from the token's `groups` claim: the admin group can do
//!   everything, the viewer group can look, and device groups can name more
//!   groups that see only their devices. Nobody else gets in.
//! - The session is a random token in an HttpOnly cookie; the database keeps
//!   only its hash. Changes need the session's CSRF token in a header.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Extension;
use axum::extract::{FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use freesize_proto::keys::{random, random_id, sha256_hex};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::config::OidcSettings;
use crate::db::now;
use crate::error::ApiError;
use crate::web::page;
use crate::{AppState, audit};

pub const SESSION_COOKIE: &str = "disktree_session";
pub const CSRF_HEADER: &str = "x-csrf-token";
const SESSION_SECONDS: i64 = 12 * 60 * 60;
/// How long a sign-in may take at the provider.
const LOGIN_SECONDS: i64 = 10 * 60;
/// How long the provider's settings and keys are kept before asking again.
const PROVIDER_TTL: Duration = Duration::from_secs(60 * 60);

/// The identity provider, as far as the Hub needs it.
pub struct Oidc {
    settings: Result<OidcSettings, String>,
    provider: tokio::sync::Mutex<Option<Arc<Provider>>>,
    http: ureq::Agent,
}

/// What the provider publishes about itself.
pub struct Provider {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub end_session_endpoint: Option<String>,
    pub jwks: JwkSet,
    fetched: Instant,
}

#[derive(Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    end_session_endpoint: Option<String>,
}

impl Oidc {
    pub fn new(settings: Result<OidcSettings, String>) -> Oidc {
        let http = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .into();
        Oidc {
            settings,
            provider: tokio::sync::Mutex::new(None),
            http,
        }
    }

    /// The settings, or which are missing.
    pub fn settings(&self) -> Result<&OidcSettings, &str> {
        self.settings.as_ref().map_err(String::as_str)
    }

    /// The provider's settings and keys, fetched again when old or when
    /// `refresh` is set (a token named a key we don't know yet).
    pub async fn provider(&self, refresh: bool) -> Result<Arc<Provider>, String> {
        let settings = self.settings()?.clone();
        let mut cached = self.provider.lock().await;
        if let Some(provider) = cached.as_ref()
            && !refresh
            && provider.fetched.elapsed() < PROVIDER_TTL
        {
            return Ok(provider.clone());
        }
        let http = self.http.clone();
        let fetched = tokio::task::spawn_blocking(move || discover(&http, &settings.issuer))
            .await
            .map_err(|e| e.to_string())??;
        let fetched = Arc::new(fetched);
        *cached = Some(fetched.clone());
        Ok(fetched)
    }
}

fn get_json<T: serde::de::DeserializeOwned>(http: &ureq::Agent, url: &str) -> Result<T, String> {
    let mut response = http
        .get(url)
        .header("Accept", "application/json")
        .call()
        .map_err(|e| format!("{url}: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("{url}: HTTP {}", response.status()));
    }
    response
        .body_mut()
        .read_json()
        .map_err(|e| format!("{url}: {e}"))
}

fn discover(http: &ureq::Agent, issuer: &str) -> Result<Provider, String> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    let found: Discovery = get_json(http, &url)?;
    // The provider must say it is the issuer we were told to trust.
    if found.issuer.trim_end_matches('/') != issuer.trim_end_matches('/') {
        return Err(format!(
            "the provider calls itself {}, not {issuer}",
            found.issuer
        ));
    }
    let published: PublishedKeys = get_json(http, &found.jwks_uri)?;
    let jwks = JwkSet {
        keys: published.keys,
    };
    Ok(Provider {
        issuer: found.issuer,
        authorization_endpoint: found.authorization_endpoint,
        token_endpoint: found.token_endpoint,
        end_session_endpoint: found.end_session_endpoint,
        jwks,
        fetched: Instant::now(),
    })
}

/// The provider's published keys. One without a signing key (Authentik's
/// default) publishes `{}` and signs ID tokens with the client secret.
#[derive(Deserialize)]
struct PublishedKeys {
    #[serde(default)]
    keys: Vec<jsonwebtoken::jwk::Jwk>,
}

/// What the Hub reads from an ID token.
#[derive(Clone, Debug, Deserialize)]
pub struct IdClaims {
    pub sub: String,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub groups: Vec<String>,
}

impl IdClaims {
    fn display_name(&self) -> String {
        [&self.name, &self.preferred_username, &self.email]
            .into_iter()
            .flatten()
            .find(|s| !s.trim().is_empty())
            .cloned()
            .unwrap_or_else(|| self.sub.clone())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TokenError {
    /// The token names a key the provider hasn't published (yet).
    UnknownKey,
    Invalid(String),
}

/// Checks an ID token and returns its claims.
pub fn validate_id_token(
    token: &str,
    jwks: &JwkSet,
    issuer: &str,
    settings: &OidcSettings,
    nonce: &str,
) -> Result<IdClaims, TokenError> {
    let invalid = |e: jsonwebtoken::errors::Error| TokenError::Invalid(e.to_string());
    let header = decode_header(token).map_err(invalid)?;
    let key = match header.alg {
        // Providers without a signing key sign with the client secret.
        Algorithm::HS256 | Algorithm::HS384 | Algorithm::HS512 => {
            DecodingKey::from_secret(settings.client_secret.as_bytes())
        }
        _ => {
            let jwk = match &header.kid {
                Some(kid) => jwks.find(kid),
                None if jwks.keys.len() == 1 => jwks.keys.first(),
                None => None,
            }
            .ok_or(TokenError::UnknownKey)?;
            DecodingKey::from_jwk(jwk).map_err(invalid)?
        }
    };
    let mut validation = Validation::new(header.alg);
    validation.set_audience(&[&settings.client_id]);
    validation.set_issuer(&[issuer]);
    validation.leeway = 60;
    validation.required_spec_claims = ["exp", "iss", "aud", "sub"]
        .into_iter()
        .map(String::from)
        .collect();
    let data = decode::<IdClaims>(token, &key, &validation).map_err(invalid)?;
    if data.claims.nonce.as_deref() != Some(nonce) {
        return Err(TokenError::Invalid("the nonce doesn't match".into()));
    }
    Ok(data.claims)
}

/// Where to go after signing in: only paths on this Hub.
fn safe_next(next: Option<&str>) -> String {
    match next {
        Some(next) if next.starts_with('/') && !next.starts_with("//") && !next.contains('\\') => {
            next.to_string()
        }
        _ => "/".to_string(),
    }
}

#[derive(Deserialize)]
pub struct LoginQuery {
    next: Option<String>,
}

/// `GET /auth/login`: off to the provider.
pub async fn login(
    State(state): State<AppState>,
    via: Option<Extension<crate::Via>>,
    Query(query): Query<LoginQuery>,
) -> Response {
    let settings = match state.oidc.settings() {
        Ok(settings) => settings.clone(),
        Err(missing) => return setup_page(missing),
    };
    let peer = via.map(|v| v.peer.ip());
    if state.login_limiter.blocked(peer) {
        return too_many();
    }
    state.login_limiter.record(peer);
    let provider = match state.oidc.provider(false).await {
        Ok(provider) => provider,
        Err(err) => {
            tracing::warn!("sign-in provider: {err}");
            return page(
                StatusCode::BAD_GATEWAY,
                "Can't reach the sign-in provider",
                &format!("The Hub couldn't read the provider's settings: {err}"),
            );
        }
    };
    let login_state = random_id::<16>();
    let nonce = random_id::<16>();
    let verifier = URL_SAFE_NO_PAD.encode(random::<32>());
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let next = safe_next(query.next.as_deref());
    let stored = {
        let (login_state, nonce, verifier) = (login_state.clone(), nonce.clone(), verifier.clone());
        state
            .db
            .call(move |c| {
                c.execute(
                    "DELETE FROM logins WHERE created < ?1",
                    [now() - LOGIN_SECONDS],
                )?;
                c.execute(
                    "INSERT INTO logins (state, verifier, nonce, next, created)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![login_state, verifier, nonce, next, now()],
                )
            })
            .await
    };
    if let Err(err) = stored {
        return ApiError::from(err).into_response();
    }
    let enc = freesize_proto::pairing::encode;
    let separator = if provider.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let url = format!(
        "{}{separator}response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&nonce={}\
         &code_challenge={}&code_challenge_method=S256",
        provider.authorization_endpoint,
        enc(&settings.client_id),
        enc(&settings.redirect_uri),
        enc("openid profile email"),
        enc(&login_state),
        enc(&nonce),
        enc(&challenge),
    );
    Redirect::to(&url).into_response()
}

/// Too many sign-in attempts from this address, or from everywhere.
fn too_many() -> Response {
    page(
        StatusCode::TOO_MANY_REQUESTS,
        "Too many sign-in attempts",
        "Wait a minute, then try again.",
    )
}

fn setup_page(missing: &str) -> Response {
    page(
        StatusCode::SERVICE_UNAVAILABLE,
        "Sign-in isn't set up yet",
        &format!(
            "The Hub signs people in through an OpenID Connect provider such as Authentik. \
             Set these and restart the Hub ({missing}). The README's \"FreeSize Hub\" section \
             walks through creating the provider and application in Authentik."
        ),
    )
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: String,
}

/// `GET /auth/callback`: back from the provider.
pub async fn callback(
    State(state): State<AppState>,
    via: Option<Extension<crate::Via>>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let peer = via.map(|v| v.peer.ip());
    if state.login_limiter.blocked(peer) {
        return too_many();
    }
    let refused = |message: &str| {
        // A failed callback counts like a sign-in start.
        state.login_limiter.record(peer);
        page(StatusCode::FORBIDDEN, "Couldn't sign you in", message)
    };
    if let Some(error) = query.error {
        let detail = query.error_description.unwrap_or_default();
        return refused(&format!("The sign-in provider said: {error} {detail}"));
    }
    let (Some(code), Some(login_state)) = (query.code, query.state) else {
        return refused("The sign-in provider sent nothing back.");
    };
    let Ok(settings) = state.oidc.settings().cloned() else {
        return refused("Sign-in isn't set up.");
    };
    let pending: Option<(String, String, String, i64)> = state
        .db
        .call(move |c| {
            c.query_row(
                "DELETE FROM logins WHERE state = ?1 RETURNING verifier, nonce, next, created",
                [login_state],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()
        })
        .await
        .ok()
        .flatten();
    let Some((verifier, nonce, next, created)) = pending else {
        return refused("This sign-in link was already used or is unknown. Try again.");
    };
    if now() - created > LOGIN_SECONDS {
        return refused("The sign-in took too long. Try again.");
    }
    let provider = match state.oidc.provider(false).await {
        Ok(provider) => provider,
        Err(err) => return refused(&format!("The sign-in provider can't be reached: {err}")),
    };

    // Swap the code for tokens.
    let http = state.oidc.http.clone();
    let token_endpoint = provider.token_endpoint.clone();
    let form_settings = settings.clone();
    let exchanged = tokio::task::spawn_blocking(move || -> Result<TokenResponse, String> {
        let mut response = http
            .post(&token_endpoint)
            .header("Accept", "application/json")
            .send_form([
                ("grant_type", "authorization_code"),
                ("code", code.as_str()),
                ("redirect_uri", form_settings.redirect_uri.as_str()),
                ("client_id", form_settings.client_id.as_str()),
                ("client_secret", form_settings.client_secret.as_str()),
                ("code_verifier", verifier.as_str()),
            ])
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let body = response.body_mut().read_to_string().unwrap_or_default();
            return Err(format!("HTTP {}: {body}", response.status()));
        }
        response.body_mut().read_json().map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())
    .and_then(|r| r);
    let tokens = match exchanged {
        Ok(tokens) => tokens,
        Err(err) => {
            tracing::warn!("token exchange: {err}");
            return refused("The sign-in provider didn't accept the sign-in. Try again.");
        }
    };

    let mut checked = validate_id_token(
        &tokens.id_token,
        &provider.jwks,
        &provider.issuer,
        &settings,
        &nonce,
    );
    if matches!(checked, Err(TokenError::UnknownKey)) {
        // The provider may have rotated its keys.
        if let Ok(fresh) = state.oidc.provider(true).await {
            checked = validate_id_token(
                &tokens.id_token,
                &fresh.jwks,
                &fresh.issuer,
                &settings,
                &nonce,
            );
        }
    }
    let claims = match checked {
        Ok(claims) => claims,
        Err(err) => {
            tracing::warn!("ID token rejected: {err:?}");
            return refused("The sign-in provider's answer didn't check out.");
        }
    };

    let config = state.config.clone();
    let created = state
        .db
        .call(move |c| create_session(c, &config, &claims))
        .await;
    match created {
        Ok(Some(token)) => {
            let mut response = Redirect::to(&next).into_response();
            set_cookie(
                response.headers_mut(),
                &token,
                SESSION_SECONDS,
                state.config.https(),
            );
            response
        }
        Ok(None) => page(
            StatusCode::FORBIDDEN,
            "No access to FreeSize",
            &format!(
                "You signed in, but your account isn't in a group that may use the Hub. An \
                 admin can add you to \"{}\" or \"{}\" in the sign-in provider.",
                state.config.admin_group, state.config.viewer_group
            ),
        ),
        Err(err) => ApiError::from(err).into_response(),
    }
}

/// Stores a session for someone who signed in, if their groups let them in.
/// Returns the session token.
fn create_session(
    c: &rusqlite::Connection,
    config: &crate::Config,
    claims: &IdClaims,
) -> rusqlite::Result<Option<String>> {
    let admin = config.is_admin(&claims.groups);
    let viewer = config.is_viewer(&claims.groups)
        || scoped_groups(c)?.iter().any(|g| claims.groups.contains(g));
    let name = claims.display_name();
    if !admin && !viewer {
        audit::append(
            c,
            &name,
            "sign-in refused",
            None,
            &json!({"subject": claims.sub, "groups": claims.groups}),
        )?;
        return Ok(None);
    }
    let token = URL_SAFE_NO_PAD.encode(random::<32>());
    c.execute("DELETE FROM sessions WHERE expires < ?1", [now()])?;
    c.execute(
        "INSERT INTO sessions (hash, subject, name, email, groups, admin, csrf, created, expires)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            sha256_hex(token.as_bytes()),
            claims.sub,
            name,
            claims.email.clone().unwrap_or_default(),
            serde_json::to_string(&claims.groups).unwrap_or_default(),
            admin,
            random_id::<16>(),
            now(),
            now() + SESSION_SECONDS,
        ],
    )?;
    audit::append(
        c,
        &name,
        "signed in",
        None,
        &json!({"subject": claims.sub, "role": if admin { "admin" } else { "viewer" }}),
    )?;
    Ok(Some(token))
}

/// Sign-in groups named by device groups, whose members see only those
/// devices.
pub fn scoped_groups(c: &rusqlite::Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = c.prepare("SELECT viewer_groups FROM device_groups")?;
    let lists = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut groups = Vec::new();
    for list in lists {
        groups.extend(serde_json::from_str::<Vec<String>>(&list?).unwrap_or_default());
    }
    Ok(groups)
}

fn set_cookie(headers: &mut HeaderMap, token: &str, max_age: i64, secure: bool) {
    let cookie = format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    );
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        headers.append(header::SET_COOKIE, value);
    }
}

fn cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v.to_string())
}

/// Someone signed in to the dashboard.
#[derive(Clone, Debug)]
pub struct User {
    pub subject: String,
    pub name: String,
    pub email: String,
    pub groups: Vec<String>,
    pub admin: bool,
    pub csrf: String,
    session: String,
}

impl User {
    /// Whether this person may look at devices of a group whose viewer
    /// groups are `viewer_groups`.
    pub fn sees(&self, viewer_groups: &[String], config: &crate::Config) -> bool {
        self.admin
            || config.is_viewer(&self.groups)
            || viewer_groups.iter().any(|g| self.groups.contains(g))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl FromRequestParts<AppState> for User {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<User, ApiError> {
        let Some(token) = cookie(&parts.headers, SESSION_COOKIE) else {
            return Err(ApiError::unauthorized("not signed in"));
        };
        let hash = sha256_hex(token.as_bytes());
        let user = state
            .db
            .call(move |c| {
                c.query_row(
                    "SELECT subject, name, email, groups, admin, csrf, hash FROM sessions
                     WHERE hash = ?1 AND expires > ?2",
                    params![hash, now()],
                    |r| {
                        Ok(User {
                            subject: r.get(0)?,
                            name: r.get(1)?,
                            email: r.get(2)?,
                            groups: serde_json::from_str(&r.get::<_, String>(3)?)
                                .unwrap_or_default(),
                            admin: r.get(4)?,
                            csrf: r.get(5)?,
                            session: r.get(6)?,
                        })
                    },
                )
                .optional()
            })
            .await?
            .ok_or_else(|| ApiError::unauthorized("not signed in"))?;
        let safe = matches!(parts.method, Method::GET | Method::HEAD | Method::OPTIONS);
        if !safe {
            let sent = parts
                .headers
                .get(CSRF_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default();
            if !constant_time_eq(sent.as_bytes(), user.csrf.as_bytes()) {
                return Err(ApiError::forbidden("missing or wrong CSRF token"));
            }
        }
        Ok(user)
    }
}

/// An admin signed in to the dashboard.
pub struct Admin(pub User);

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Admin, ApiError> {
        let user = User::from_request_parts(parts, state).await?;
        if !user.admin {
            return Err(ApiError::forbidden("only admins can do that"));
        }
        Ok(Admin(user))
    }
}

/// `POST /auth/logout`. Answers where to go next: the provider's sign-out
/// page if it has one.
pub async fn logout(State(state): State<AppState>, user: User) -> Response {
    let session = user.session.clone();
    let name = user.name.clone();
    let removed = state
        .db
        .call(move |c| -> rusqlite::Result<()> {
            c.execute("DELETE FROM sessions WHERE hash = ?1", [session])?;
            audit::append(c, &name, "signed out", None, &json!({}))
        })
        .await;
    if let Err(err) = removed {
        return ApiError::from(err).into_response();
    }
    let redirect = match state.oidc.provider(false).await {
        Ok(provider) => provider
            .end_session_endpoint
            .clone()
            .unwrap_or_else(|| "/".into()),
        Err(_) => "/".into(),
    };
    let mut response = axum::Json(json!({ "redirect": redirect })).into_response();
    set_cookie(response.headers_mut(), "", 0, state.config.https());
    response
}

#[cfg(test)]
mod tests {
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::json;

    use super::*;

    fn settings() -> OidcSettings {
        OidcSettings {
            issuer: "https://auth.lan/application/o/freesize/".into(),
            client_id: "freesize".into(),
            client_secret: "s3cret-s3cret-s3cret-s3cret-s3cret".into(),
            redirect_uri: "https://hub.lan/auth/callback".into(),
        }
    }

    fn token(claims: serde_json::Value, secret: &str) -> String {
        encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }

    #[test]
    fn providers_without_keys_publish_an_empty_set() {
        let keys: PublishedKeys = serde_json::from_str("{}").unwrap();
        assert!(keys.keys.is_empty());
        let keys: PublishedKeys = serde_json::from_value(json!({ "keys": [{
            "kty": "RSA", "kid": "k1", "use": "sig", "alg": "RS256",
            "n": "sXchDaQebHnPiGvyDOAT4saGEUetSyo9MKLOoWFsueri23bOdgWp4Dy1WlUzewbgBHod5pcM9H95GQRV3JDXboIRROSBigeC5yjU1hGzHHyXss8UDprecbAYxknTcQkhslANGRUZmdTOQ5qTRsLAt6BTYuyvVRdhS8exSZEy_c4gs_7svlJJQ4H9_NxsiIoLwAEk7-Q3UXERGYw_75IDrGA84-lA_-Ct4eTlXHBIY2EaV7t7LjJaynVJCpkv4LKjTTAumiGUIuQhrNhZLuF_RJLqHpM2kgWFLU7-VTdL1VbC2tejvcI2BlMkEpk1BzBZI0KQB0GaDWFLN-aEAw3vRw",
            "e": "AQAB"
        }]}))
        .unwrap();
        assert_eq!(keys.keys.len(), 1);
    }

    #[test]
    fn id_tokens_are_checked() {
        let s = settings();
        let empty = JwkSet { keys: vec![] };
        let good = json!({
            "iss": s.issuer, "aud": "freesize", "sub": "u1", "exp": now() + 300,
            "nonce": "n1", "name": "Robert", "groups": ["freesize-admins"]
        });
        let claims = validate_id_token(
            &token(good.clone(), &s.client_secret),
            &empty,
            &s.issuer,
            &s,
            "n1",
        )
        .unwrap();
        assert_eq!(claims.display_name(), "Robert");
        assert_eq!(claims.groups, ["freesize-admins"]);

        let check = |claims: serde_json::Value, secret: &str, nonce: &str| {
            validate_id_token(&token(claims, secret), &empty, &s.issuer, &s, nonce).is_err()
        };
        assert!(check(
            good.clone(),
            "another secret entirely, long enough",
            "n1"
        ));
        assert!(check(good.clone(), &s.client_secret, "n2"));
        let mut other_audience = good.clone();
        other_audience["aud"] = json!("someone-else");
        assert!(check(other_audience, &s.client_secret, "n1"));
        let mut other_issuer = good.clone();
        other_issuer["iss"] = json!("https://evil.example/");
        assert!(check(other_issuer, &s.client_secret, "n1"));
        let mut expired = good;
        expired["exp"] = json!(now() - 3600);
        assert!(check(expired, &s.client_secret, "n1"));
    }

    #[test]
    fn redirects_stay_on_the_hub() {
        assert_eq!(safe_next(Some("/#/devices")), "/#/devices");
        assert_eq!(safe_next(Some("//evil.example")), "/");
        assert_eq!(safe_next(Some("https://evil.example")), "/");
        assert_eq!(safe_next(Some("/\\evil.example")), "/");
        assert_eq!(safe_next(None), "/");
    }

    #[test]
    fn cookies_are_found() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("a=1; disktree_session=abc; b=2"),
        );
        assert_eq!(cookie(&headers, SESSION_COOKIE).as_deref(), Some("abc"));
        assert_eq!(cookie(&headers, "c"), None);
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
