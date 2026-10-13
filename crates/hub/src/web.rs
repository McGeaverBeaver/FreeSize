//! The dashboard's files, built into the binary, and the security headers
//! every response gets.

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{Html, IntoResponse, Response};

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
