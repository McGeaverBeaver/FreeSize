//! The dashboard's files, built into the binary, and the security headers
//! every response gets.

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Redirect, Response};

use crate::{AppState, Via};

const INDEX: &str = include_str!("../web/index.html");
const SCRIPT: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/app.css");
const ICON: &str = include_str!("../web/icon.svg");

fn file(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            // Small files; always check for a newer Hub.
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

pub async fn index() -> Response {
    file("text/html; charset=utf-8", INDEX)
}

pub async fn script() -> Response {
    file("text/javascript; charset=utf-8", SCRIPT)
}

pub async fn style() -> Response {
    file("text/css; charset=utf-8", STYLE)
}

pub async fn icon() -> Response {
    file("image/svg+xml", ICON)
}

pub fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A plain page with a title and a message, for sign-in problems.
pub fn page(status: StatusCode, title: &str, message: &str) -> Response {
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title} · FreeSize Hub</title><link rel=\"stylesheet\" href=\"/app.css\">\
         <link rel=\"icon\" href=\"/icon.svg\"></head><body class=\"plain\"><main class=\"notice\">\
         <h1>{title}</h1><p>{message}</p><p><a class=\"button\" href=\"/\">Back to the Hub</a></p>\
         </main></body></html>",
        title = escape(title),
        message = escape(message),
    );
    (status, Html(body)).into_response()
}

/// `GET /ca.pem`: the Hub's own certificate authority, to install on a
/// computer so its browser trusts the dashboard. Public, like any
/// certificate.
pub async fn authority(State(state): State<AppState>) -> Response {
    match state.tls.as_ref().and_then(|t| t.ca_pem()) {
        Some(pem) => (
            [
                (header::CONTENT_TYPE, "application/x-pem-file"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=\"freesize-hub-authority.pem\"",
                ),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            pem.to_string(),
        )
            .into_response(),
        None => page(
            StatusCode::NOT_FOUND,
            "No authority",
            "This Hub doesn't use a certificate of its own.",
        ),
    }
}

/// Sends the dashboard over to HTTPS when it was asked for over plain http
/// although the Hub serves HTTPS, on the same port. Devices' requests
/// (`/api/v1/`) are answered as they are, for devices paired before the Hub
/// served HTTPS, which are told to move in their poll; `/api/status` too,
/// for the container's health check.
pub async fn https_only(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let plain = request.extensions().get::<Via>().is_some_and(|v| !v.tls);
    let path = request.uri().path();
    let host = request
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .map(String::from)
        .or_else(|| {
            state
                .config
                .public_url()
                .and_then(|u| u.strip_prefix("https://").map(String::from))
        });
    if plain
        && state.tls.is_some()
        && !path.starts_with("/api/v1/")
        && path != "/api/status"
        && let Some(host) = host
    {
        let target = format!(
            "https://{host}{}",
            request.uri().path_and_query().map_or("/", |p| p.as_str())
        );
        return Redirect::permanent(&target).into_response();
    }
    next.run(request).await
}

/// Adds security headers: no framing, no sniffing, scripts and styles only
/// from the Hub itself.
pub async fn security_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    let set = |headers: &mut axum::http::HeaderMap, name, value: &'static str| {
        headers
            .entry(name)
            .or_insert(HeaderValue::from_static(value));
    };
    set(
        headers,
        header::CONTENT_SECURITY_POLICY,
        "default-src 'self'; img-src 'self' data:; script-src 'self'; style-src 'self'; \
         connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
    );
    set(headers, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(headers, header::X_FRAME_OPTIONS, "DENY");
    set(headers, header::REFERRER_POLICY, "same-origin");
    response
}
