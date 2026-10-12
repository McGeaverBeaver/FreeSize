//! A stand-in OpenID Connect provider for tests and for trying the dashboard
//! without Authentik. It signs ID tokens with RS256 like Authentik's default
//! setup, checks PKCE and the client secret, and signs in whoever
//! `MockIdp::user` says.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Form, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

/// A throwaway key, only ever used by this mock.
const KEY_PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEpAIBAAKCAQEApunrvnhuDgA2HFEJPoRLDp63DXWl5UR0FylPnY9bB5/8s/wQ\nEx7dr14kYbBVoyG1CEeCVr97eGXvXYny+boYMIaFrS2ASsfCSqizFXto/2ptoKv/\nIo+dbv6FvlYllszePnv7dtVmWyLS7RBvVd6MJPQUjX3REMPsvqSCNYqKqweO3zFW\nTfjN2gc43399fAo/Gbw16o64qPgszX/b3SDMPEgO+ACHjAhqQRwtnoIptXdvXcey\nryO94Byml1j1N3IEKW+obvn2/KB8uwfm84ms184GGIBovPB70BirhYdG85gfUslP\n4sZ8iVmhMIKF9JvHgYapV5OGVhKcrnlMb8APKQIDAQABAoIBACn9zIShGJLmI7bQ\n/kHfl2BotoWo1OEObSRMjM6HO6/55GHFZqfgyoWtafq3mXvOZZRdJSG8z4u1HViL\ndXi1BBPlK+kq+s0VX0BYG6GqZ/pl5ImY67lepw0+9Bez5YYAKWNP3q4BNtxzsNva\nGCBiuiAr1aXZAsk0eTFnAqkEnykyd+QTRvltMKPDE8inJNm2Pc84ffTjyT1qFEaJ\ndaFlsvISh4/NubAJJicFLQLWlGenTvzaMerQt5SrzQCfq6T/mtn7dr762bmTGCpQ\nMO2Yhp+QbfwzQWvdxV0EvPwDgwRWHyJ1YiwHYe4j9R/6fCHSSwgOCevkyfILLaDG\n2fFKOB8CgYEA2x1f2mvrE2e0wAzQgAAZQ5UPTn9lcEJyBoP7S6AGZdpGx3w+Evqr\nAfNNJuH/9ji8NVbA5rx4tk+mu2JNNlKGaZIv93ey9GcR0RysfNj1aROgyRnODrwZ\nUlf99dA61bgyaKJCDmgKncFg0wUQAqICxl4PRGgy/PqeHrtpcEEk/zsCgYEAwwL5\ncRC7lQcHHn/VQ5MrDYW2BfY9ZVrlrQsghVtGSgjDsolNZ+TRGnMJggkODFsUyp6N\nBc0RTJdHXcwI4HX2DE0CfQd+ZjDGYJIX0nKv9hlrLKKiRYV55WEPkbaAguds9RdK\n/mmLcz/HgNbDBqzzL+J6NnjJekp9bs28Tm1JDOsCgYEAxZES4Ir2ByU3IQNs7D0q\ninlYt9q+yB3hWmaeq2261xOdRjmMbuh+UN/j7O9SL9ywNxfmDyZZYWkoWUUlgZmf\ngIlDnsrbHOvicL59+IRXo0sDatBNr5OC8XBNmNsMcbAII7hJ/1toVx4KA7tI2Fbk\nUaB1+6RIdTo0YFlzlLfoeAkCgYEAiSayYH6wLuO+cgjd6KS8hsofdpn0HDCNj+Jf\ngOrsyGzZldtTAVrAZaW6biYIJ1B3kvlnnUjYfdF3c9Rn+dV8srIGqgB3OU/cdVbK\nQkw+zQqbgczv640wac6VsJl2UJqK2uUC7WM1nFyi504j47IHZwoVgfH5YXS6Ow+e\n/FmIIvkCgYBil/nb9r7uWA2N9hs0XLI/mraGWN+dPWSkzpTZV8ifRJxiI59Y4Qt6\nfJqQBXJcox+K5vniFPeQVGld2pm1bCaeGv6PldeH7uOqtr2pGksJ/Eqd9OBiwrBD\nA5DCFwBzP3h2ntpOwNmPfV6fJ6+sL4Nc07e+XWDHjsgy5Qz155I+4Q==\n-----END RSA PRIVATE KEY-----";
const KEY_N: &str = "punrvnhuDgA2HFEJPoRLDp63DXWl5UR0FylPnY9bB5_8s_wQEx7dr14kYbBVoyG1CEeCVr97eGXvXYny-boYMIaFrS2ASsfCSqizFXto_2ptoKv_Io-dbv6FvlYllszePnv7dtVmWyLS7RBvVd6MJPQUjX3REMPsvqSCNYqKqweO3zFWTfjN2gc43399fAo_Gbw16o64qPgszX_b3SDMPEgO-ACHjAhqQRwtnoIptXdvXceyryO94Byml1j1N3IEKW-obvn2_KB8uwfm84ms184GGIBovPB70BirhYdG85gfUslP4sZ8iVmhMIKF9JvHgYapV5OGVhKcrnlMb8APKQ";

pub const CLIENT_ID: &str = "disktree";
pub const CLIENT_SECRET: &str = "mock-client-secret-0123456789abcdef";

#[derive(Clone, Debug)]
pub struct MockUser {
    pub sub: String,
    pub name: String,
    pub email: String,
    pub groups: Vec<String>,
}

impl MockUser {
    pub fn new(sub: &str, name: &str, groups: &[&str]) -> MockUser {
        MockUser {
            sub: sub.into(),
            name: name.into(),
            email: format!("{sub}@example.lan"),
            groups: groups.iter().map(|g| g.to_string()).collect(),
        }
    }
}

struct Grant {
    nonce: String,
    challenge: String,
    redirect_uri: String,
    user: MockUser,
}

#[derive(Clone)]
pub struct MockIdp {
    /// The issuer, with a trailing slash like Authentik's.
    pub issuer: String,
    pub user: Arc<Mutex<MockUser>>,
    grants: Arc<Mutex<HashMap<String, Grant>>>,
}

impl MockIdp {
    /// Starts the provider on a random local port.
    pub async fn start() -> MockIdp {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let issuer = format!(
            "http://{}/application/o/disktree/",
            listener.local_addr().unwrap()
        );
        Self::serve(listener, issuer).await
    }

    pub async fn serve(listener: tokio::net::TcpListener, issuer: String) -> MockIdp {
        let idp = MockIdp {
            issuer,
            user: Arc::new(Mutex::new(MockUser::new(
                "u-admin",
                "Robert",
                &["disktree-admins"],
            ))),
            grants: Arc::default(),
        };
        let app = Router::new()
            .route(
                "/application/o/disktree/.well-known/openid-configuration",
                get(discovery),
            )
            .route("/application/o/authorize/", get(authorize))
            .route("/application/o/token/", post(token))
            .route("/application/o/disktree/jwks/", get(jwks))
            .with_state(idp.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        idp
    }

    pub fn set_user(&self, user: MockUser) {
        *self.user.lock().unwrap() = user;
    }

    fn base(&self) -> String {
        self.issuer.trim_end_matches("/disktree/").to_string()
    }
}

async fn discovery(State(idp): State<MockIdp>) -> impl IntoResponse {
    let base = idp.base();
    axum::Json(json!({
        "issuer": idp.issuer,
        "authorization_endpoint": format!("{base}/authorize/"),
        "token_endpoint": format!("{base}/token/"),
        "jwks_uri": format!("{}jwks/", idp.issuer),
        "end_session_endpoint": format!("{}end-session/", idp.issuer),
        "response_types_supported": ["code"],
        "id_token_signing_alg_values_supported": ["RS256"],
    }))
}

async fn jwks() -> impl IntoResponse {
    axum::Json(json!({"keys": [{
        "kty": "RSA", "kid": "mock-key", "use": "sig", "alg": "RS256", "n": KEY_N, "e": "AQAB"
    }]}))
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    client_id: String,
    redirect_uri: String,
    state: String,
    nonce: String,
    code_challenge: String,
    code_challenge_method: String,
    response_type: String,
    scope: String,
}

async fn authorize(State(idp): State<MockIdp>, Query(q): Query<AuthorizeQuery>) -> Response {
    if q.client_id != CLIENT_ID
        || q.response_type != "code"
        || q.code_challenge_method != "S256"
        || !q.scope.split(' ').any(|s| s == "openid")
    {
        return (StatusCode::BAD_REQUEST, "bad authorize request").into_response();
    }
    let code = format!("code-{}", rand_hex());
    let user = idp.user.lock().unwrap().clone();
    idp.grants.lock().unwrap().insert(
        code.clone(),
        Grant {
            nonce: q.nonce,
            challenge: q.code_challenge,
            redirect_uri: q.redirect_uri.clone(),
            user,
        },
    );
    Redirect::to(&format!("{}?code={code}&state={}", q.redirect_uri, q.state)).into_response()
}

fn rand_hex() -> String {
    disktree_proto::keys::random_id::<10>()
}

#[derive(Deserialize)]
struct TokenForm {
    grant_type: String,
    code: String,
    redirect_uri: String,
    client_id: String,
    client_secret: String,
    code_verifier: String,
}

async fn token(State(idp): State<MockIdp>, Form(f): Form<TokenForm>) -> Response {
    let Some(grant) = idp.grants.lock().unwrap().remove(&f.code) else {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    };
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(f.code_verifier.as_bytes()));
    if f.grant_type != "authorization_code"
        || f.client_id != CLIENT_ID
        || f.client_secret != CLIENT_SECRET
        || f.redirect_uri != grant.redirect_uri
        || challenge != grant.challenge
    {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "invalid_grant"})),
        )
            .into_response();
    }
    let now = disktree_hub::db::now();
    let claims = json!({
        "iss": idp.issuer, "aud": CLIENT_ID, "sub": grant.user.sub, "exp": now + 300, "iat": now,
        "nonce": grant.nonce, "name": grant.user.name, "email": grant.user.email,
        "preferred_username": grant.user.sub, "groups": grant.user.groups,
    });
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("mock-key".into());
    let key = EncodingKey::from_rsa_pem(KEY_PEM.as_bytes()).unwrap();
    let id_token = jsonwebtoken::encode(&header, &claims, &key).unwrap();
    axum::Json(json!({"access_token": "mock", "token_type": "Bearer", "id_token": id_token, "expires_in": 300}))
        .into_response()
}
