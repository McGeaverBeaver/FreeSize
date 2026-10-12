//! A Hub on a random port with the mock sign-in provider, and a small
//! "browser" that keeps the session cookie and doesn't follow redirects.

#![allow(dead_code)]

pub mod idp;

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use disktree_hub::{AppState, Config};
use serde_json::Value;

use idp::{CLIENT_ID, CLIENT_SECRET, MockIdp};

pub struct TestHub {
    pub url: String,
    pub state: AppState,
    pub idp: MockIdp,
    pub dir: PathBuf,
}

impl Drop for TestHub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub async fn start_hub(name: &str) -> TestHub {
    let idp = MockIdp::start().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let dir = std::env::temp_dir().join(format!("disktree-hub-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let config = Config::parse_from([
        "disktree-hub",
        "--data",
        dir.to_str().unwrap(),
        "--url",
        &url,
        "--oidc-issuer",
        &idp.issuer,
        "--oidc-client-id",
        CLIENT_ID,
        "--oidc-client-secret",
        CLIENT_SECRET,
    ]);
    let state = AppState::open(config).unwrap();
    tokio::spawn(disktree_hub::serve(listener, state.clone()));
    TestHub {
        url,
        state,
        idp,
        dir,
    }
}

pub struct Response {
    pub status: u16,
    pub location: Option<String>,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Response {
    pub fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    pub fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map_or("", |(_, v)| v.as_str())
    }
}

pub struct Browser {
    agent: ureq::Agent,
    pub cookie: Option<String>,
    pub csrf: Option<String>,
}

impl Browser {
    pub fn new() -> Browser {
        let agent = ureq::Agent::config_builder()
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(20)))
            .build()
            .into();
        Browser {
            agent,
            cookie: None,
            csrf: None,
        }
    }

    fn finish(&mut self, mut response: ureq::http::Response<ureq::Body>) -> Response {
        for value in response.headers().get_all("set-cookie") {
            let text = value.to_str().unwrap_or_default();
            if let Some(rest) = text.strip_prefix("disktree_session=") {
                let token = rest.split(';').next().unwrap_or_default();
                self.cookie = (!token.is_empty()).then(|| token.to_string());
            }
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        let headers = response
            .headers()
            .iter()
            .map(|(n, v)| (n.to_string(), v.to_str().unwrap_or_default().to_string()))
            .collect();
        Response {
            status: response.status().as_u16(),
            location,
            headers,
            body: response.body_mut().read_to_string().unwrap_or_default(),
        }
    }

    pub fn get(&mut self, url: &str) -> Response {
        let mut request = self.agent.get(url);
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", &format!("disktree_session={cookie}"));
        }
        let response = request.call().unwrap();
        self.finish(response)
    }

    /// A POST with JSON; `csrf` adds the session's CSRF header.
    pub fn post(&mut self, url: &str, body: &Value, csrf: bool) -> Response {
        let mut request = self
            .agent
            .post(url)
            .header("Content-Type", "application/json");
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", &format!("disktree_session={cookie}"));
        }
        if csrf && let Some(token) = &self.csrf {
            request = request.header("X-CSRF-Token", token);
        }
        let response = request.send(body.to_string()).unwrap();
        self.finish(response)
    }

    /// A POST of raw bytes, as a file upload sends them.
    pub fn post_bytes(&mut self, url: &str, body: &[u8]) -> Response {
        let mut request = self
            .agent
            .post(url)
            .header("Content-Type", "application/octet-stream");
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", &format!("disktree_session={cookie}"));
        }
        if let Some(token) = &self.csrf {
            request = request.header("X-CSRF-Token", token);
        }
        let response = request.send(body).unwrap();
        self.finish(response)
    }

    pub fn delete(&mut self, url: &str) -> Response {
        let mut request = self.agent.delete(url);
        if let Some(cookie) = &self.cookie {
            request = request.header("Cookie", &format!("disktree_session={cookie}"));
        }
        if let Some(token) = &self.csrf {
            request = request.header("X-CSRF-Token", token);
        }
        let response = request.call().unwrap();
        self.finish(response)
    }

    /// Goes through the whole sign-in; returns where the Hub sent us at the
    /// end.
    pub fn sign_in(&mut self, hub_url: &str, next: &str) -> Response {
        let start = self.get(&format!("{hub_url}/auth/login?next={next}"));
        assert_eq!(start.status, 303, "login should redirect: {}", start.body);
        let at_provider = self.get(start.location.as_deref().unwrap());
        assert_eq!(at_provider.status, 303, "provider: {}", at_provider.body);
        let back = self.get(at_provider.location.as_deref().unwrap());
        if back.status == 303 {
            let me = self.get(&format!("{hub_url}/api/me"));
            self.csrf = me.json()["csrf"].as_str().map(String::from);
        }
        back
    }
}

/// Runs blocking browser steps off the async test's thread.
pub async fn blocking<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    tokio::task::spawn_blocking(f).await.unwrap()
}
