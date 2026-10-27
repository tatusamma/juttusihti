//! Sign in with ChatGPT for open-source, locally run apps, so OpenAI runs
//! can be charged to the user's ChatGPT plan instead of an API key.
//!
//! Flow (developers.openai.com/siwc/token-sharing-open-source):
//! 1. Authorization Code + PKCE (S256) in the system browser, with a
//!    loopback redirect on `127.0.0.1` (only the port varies). The first
//!    sign-in uses `client_id=dynamic_agent_client` and receives an issued
//!    `oaiapp_…` client id in the callback, which later sign-ins reuse.
//! 2. Code exchange at `/api/accounts/oauth/token` with the PKCE verifier
//!    and `resource=https://api.openai.com/v1`; no client secret exists.
//! 3. The granted scopes must include `chatgpt.tokens.use.direct`; a
//!    valid ID token alone does not enable plan usage.
//! 4. Access tokens last one hour. Refresh tokens rotate on every refresh
//!    and are persisted immediately; refreshes are serialized.
//!
//! The ID token's claims (`iss`, `aud`, `exp`, `nonce`) are checked. Its
//! signature is not: the token comes straight from the token endpoint over
//! TLS, which OpenID Connect Core §3.1.3.7 accepts in place of signature
//! validation for this case.
//!
//! Only the refresh token is stored (by the caller, in the OS secret
//! store). Access tokens stay in memory.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub const AUTH_BASE: &str = "https://auth.openai.com";
pub const API_BASE: &str = "https://api.openai.com/v1";
/// Shown on the consent page and used for every installation.
pub const APP_NAME: &str = "Juttusihti";
const SCOPES: &str = "openid profile email offline_access resource.invoke chatgpt.tokens.use.direct";
const PLAN_SCOPE: &str = "chatgpt.tokens.use.direct";
const DYNAMIC_CLIENT: &str = "dynamic_agent_client";
const CALLBACK_PATH: &str = "/auth/callback";
/// Refresh this long before the access token expires.
const EXPIRY_MARGIN: Duration = Duration::from_secs(120);
/// How long to wait for the browser to come back.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, thiserror::Error, Clone, PartialEq)]
pub enum AuthError {
    #[error("could not reach OpenAI: {0}")]
    Network(String),
    #[error("sign-in was cancelled in the browser")]
    Denied,
    #[error("sign-in timed out")]
    Timeout,
    #[error("unexpected sign-in response: {0}")]
    Protocol(String),
    #[error("this ChatGPT account did not grant plan usage to Juttusihti (a Plus or Pro plan is needed)")]
    NotEligible,
    #[error("signed in as a different ChatGPT account than before; sign out first to switch accounts")]
    DifferentAccount,
    #[error("the ChatGPT sign-in has expired or was revoked; sign in again under AI setup")]
    SignedOut,
    #[error("could not save the sign-in: {0}")]
    Store(String),
}

/// Where the OpenAI auth and API servers are (overridable for tests).
#[derive(Debug, Clone, PartialEq)]
pub struct Servers {
    pub auth_base: String,
    pub api_base: String,
}

impl Default for Servers {
    fn default() -> Self {
        Self { auth_base: AUTH_BASE.into(), api_base: API_BASE.into() }
    }
}

/// Non-secret sign-in state kept between sessions: the installation's host
/// id, and after the first sign-in the issued client id and account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub host_id: String,
    #[serde(default)]
    pub client_id: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
}

impl Registration {
    /// A fresh registration with a random, stable-per-installation host id.
    pub fn new() -> Self {
        let b = random_bytes::<16>();
        let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
        // RFC 4122 version 4 layout.
        let v =
            format!("{}-{}-4{}-{:x}{}-{}", &h[0..8], &h[8..12], &h[13..16], (b[8] & 0x3) | 0x8, &h[17..20], &h[20..32]);
        Self { host_id: format!("urn:uuid:{v}"), client_id: None, subject: None, email: None }
    }
}

impl Default for Registration {
    fn default() -> Self {
        Self::new()
    }
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("operating system random source");
    b
}

fn random_token() -> String {
    URL_SAFE_NO_PAD.encode(random_bytes::<32>())
}

fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

fn http_client() -> Result<reqwest::Client, AuthError> {
    reqwest::Client::builder()
        .user_agent(concat!("Juttusihti/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(60))
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AuthError::Network(e.to_string()))
}

fn form(pairs: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    id_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[serde(default)]
    scope: Option<String>,
}

/// Sends a form to the token endpoint. Error bodies yield their OAuth
/// `error` code.
async fn token_request(http: &reqwest::Client, servers: &Servers, body: String) -> Result<TokenResponse, TokenError> {
    let url = format!("{}/api/accounts/oauth/token", servers.auth_base.trim_end_matches('/'));
    let resp = http
        .post(url)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| TokenError::Network(e.to_string()))?;
    let status = resp.status();
    let bytes = resp.bytes().await.map_err(|e| TokenError::Network(e.to_string()))?;
    if bytes.len() > 256 * 1024 {
        return Err(TokenError::Other("token response too large".into()));
    }
    if !status.is_success() {
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        let code = v["error"]["code"].as_str().or(v["error"].as_str()).or(v["code"].as_str()).unwrap_or("").to_string();
        return if status.is_server_error() {
            Err(TokenError::Network(format!("token endpoint returned {status}")))
        } else {
            Err(TokenError::Rejected { status: status.as_u16(), code })
        };
    }
    serde_json::from_slice(&bytes).map_err(|e| TokenError::Other(e.to_string()))
}

#[derive(Debug)]
enum TokenError {
    Network(String),
    Rejected { status: u16, code: String },
    Other(String),
}

/// Refresh errors after which the stored token can never work again.
fn is_dead_refresh(code: &str) -> bool {
    matches!(
        code,
        "invalid_grant"
            | "invalid_refresh_token"
            | "token_expired"
            | "refresh_token_expired"
            | "refresh_token_invalidated"
            | "refresh_token_reused"
    )
}

fn has_plan_scope(scope: &str) -> bool {
    scope.split_whitespace().any(|s| s == PLAN_SCOPE)
}

/// Claims of the ID token this app relies on.
#[derive(Debug, Clone, PartialEq)]
struct Identity {
    subject: String,
    email: Option<String>,
}

fn check_id_token(token: &str, issuer: &str, client_id: &str, nonce: &str, now: i64) -> Result<Identity, AuthError> {
    let bad = |m: &str| AuthError::Protocol(format!("ID token {m}"));
    let payload = token.split('.').nth(1).ok_or_else(|| bad("is malformed"))?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).map_err(|_| bad("is malformed"))?;
    let c: Value = serde_json::from_slice(&bytes).map_err(|_| bad("is malformed"))?;
    if c["iss"].as_str().map(|s| s.trim_end_matches('/')) != Some(issuer.trim_end_matches('/')) {
        return Err(bad("has the wrong issuer"));
    }
    let aud_ok = match &c["aud"] {
        Value::String(a) => a == client_id,
        Value::Array(a) => a.iter().any(|x| x == client_id),
        _ => false,
    };
    if !aud_ok {
        return Err(bad("was issued to another client"));
    }
    if c["exp"].as_i64().is_none_or(|exp| exp <= now) {
        return Err(bad("has expired"));
    }
    if c["nonce"].as_str() != Some(nonce) {
        return Err(bad("does not match this sign-in"));
    }
    let subject = c["sub"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| bad("has no subject"))?.to_string();
    Ok(Identity { subject, email: c["email"].as_str().map(str::to_string) })
}

/// A completed sign-in.
#[derive(Clone)]
pub struct SignedIn {
    /// Updated registration (issued client id, account) to persist.
    pub registration: Registration,
    /// Store in the OS secret store.
    pub refresh_token: String,
    access_token: String,
    expires_at: Instant,
}

impl std::fmt::Debug for SignedIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignedIn").field("registration", &self.registration).finish_non_exhaustive()
    }
}

impl SignedIn {
    /// A session for API calls, starting with the fresh access token.
    pub fn into_session(self, servers: Servers, persist: Persist) -> Result<Session, AuthError> {
        let client_id = self.registration.client_id.clone().ok_or(AuthError::SignedOut)?;
        Session::build(servers, client_id, self.refresh_token, Some((self.access_token, self.expires_at)), persist)
    }
}

/// A browser sign-in waiting for its loopback callback.
pub struct PendingSignIn {
    listener: TcpListener,
    url: String,
    redirect_uri: String,
    verifier: String,
    state: String,
    nonce: String,
    /// Issued client id on a returning sign-in.
    returning: Option<String>,
    registration: Registration,
    servers: Servers,
    http: reqwest::Client,
}

impl PendingSignIn {
    /// Starts the loopback listener (before the browser opens) and builds
    /// the authorization URL.
    pub async fn start(servers: Servers, registration: Registration) -> Result<Self, AuthError> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| AuthError::Network(format!("loopback listener: {e}")))?;
        let port = listener.local_addr().map_err(|e| AuthError::Network(e.to_string()))?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
        let verifier = random_token();
        let (state, nonce) = (random_token(), random_token());
        let challenge = pkce_challenge(&verifier);
        let returning = registration.client_id.clone();
        let mut pairs: Vec<(&str, &str)> = vec![
            ("client_id", returning.as_deref().unwrap_or(DYNAMIC_CLIENT)),
            ("ext_agent_host_id", &registration.host_id),
            ("response_type", "code"),
            ("redirect_uri", &redirect_uri),
            ("scope", SCOPES),
            ("resource", API_BASE),
            ("state", &state),
            ("nonce", &nonce),
            ("code_challenge_method", "S256"),
            ("code_challenge", &challenge),
        ];
        match (&returning, &registration.email) {
            (None, _) => pairs.push(("agent_name_hint", APP_NAME)),
            (Some(_), Some(email)) => pairs.push(("login_hint", email)),
            (Some(_), None) => {}
        }
        let url = format!(
            "{}/api/accounts/authorize?{}",
            servers.auth_base.trim_end_matches('/'),
            form(&pairs).replace('+', "%20")
        );
        Ok(Self {
            listener,
            url,
            redirect_uri,
            verifier,
            state,
            nonce,
            returning,
            registration,
            servers,
            http: http_client()?,
        })
    }

    /// Open this in the system browser.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Waits for the browser callback, then exchanges the code.
    pub async fn finish(self) -> Result<SignedIn, AuthError> {
        let callback =
            tokio::time::timeout(SIGN_IN_TIMEOUT, self.wait_for_callback()).await.map_err(|_| AuthError::Timeout)??;
        let client_id = match (&self.returning, callback.client_id) {
            (Some(saved), Some(got)) if *saved != got => {
                return Err(AuthError::Protocol("callback named a different client".into()));
            }
            (Some(saved), _) => saved.clone(),
            (None, Some(issued)) => issued,
            (None, None) => return Err(AuthError::Protocol("no client id was issued".into())),
        };
        let body = form(&[
            ("grant_type", "authorization_code"),
            ("client_id", &client_id),
            ("code", &callback.code),
            ("code_verifier", &self.verifier),
            ("redirect_uri", &self.redirect_uri),
            ("resource", API_BASE),
        ]);
        let t = token_request(&self.http, &self.servers, body).await.map_err(|e| match e {
            TokenError::Network(m) => AuthError::Network(m),
            TokenError::Rejected { code, .. } => AuthError::Protocol(format!("code exchange rejected ({code})")),
            TokenError::Other(m) => AuthError::Protocol(m),
        })?;
        let scope = t.scope.as_deref().or(callback.scope.as_deref()).unwrap_or("");
        if !has_plan_scope(scope) {
            return Err(AuthError::NotEligible);
        }
        let id_token = t.id_token.as_deref().ok_or_else(|| AuthError::Protocol("no ID token".into()))?;
        let identity =
            check_id_token(id_token, &self.servers.auth_base, &client_id, &self.nonce, chrono::Utc::now().timestamp())?;
        if self.registration.subject.as_ref().is_some_and(|s| *s != identity.subject) {
            return Err(AuthError::DifferentAccount);
        }
        let refresh_token =
            t.refresh_token.ok_or_else(|| AuthError::Protocol("no refresh token (offline access)".into()))?;
        let mut registration = self.registration;
        registration.client_id = Some(client_id);
        registration.subject = Some(identity.subject);
        registration.email = identity.email;
        Ok(SignedIn {
            registration,
            refresh_token,
            access_token: t.access_token,
            expires_at: Instant::now() + Duration::from_secs(t.expires_in.unwrap_or(3600)),
        })
    }

    /// Serves the loopback listener until a request with this attempt's
    /// `state` arrives. Requests with another state are answered and
    /// ignored, so a stray local request cannot end the sign-in.
    async fn wait_for_callback(&self) -> Result<Callback, AuthError> {
        loop {
            let (mut sock, _) = self.listener.accept().await.map_err(|e| AuthError::Network(e.to_string()))?;
            let Some(target) = read_request_target(&mut sock).await else { continue };
            let parsed = url::Url::parse(&format!("http://127.0.0.1{target}")).ok();
            let Some(u) = parsed.filter(|u| u.path() == CALLBACK_PATH) else {
                respond(&mut sock, "404 Not Found", "Not found.").await;
                continue;
            };
            let q: std::collections::HashMap<String, String> = u.query_pairs().into_owned().collect();
            if q.get("state") != Some(&self.state) {
                respond(&mut sock, "400 Bad Request", "This sign-in link is not the one Juttusihti is waiting for.")
                    .await;
                continue;
            }
            if q.contains_key("error") {
                respond(&mut sock, "200 OK", "Sign-in was cancelled. You can close this window.").await;
                return Err(AuthError::Denied);
            }
            let Some(code) = q.get("code").cloned() else {
                respond(&mut sock, "400 Bad Request", "The sign-in response had no code.").await;
                return Err(AuthError::Protocol("callback without a code".into()));
            };
            respond(&mut sock, "200 OK", "Signed in. You can close this window and return to Juttusihti.").await;
            return Ok(Callback { code, scope: q.get("scope").cloned(), client_id: q.get("client_id").cloned() });
        }
    }
}

struct Callback {
    code: String,
    scope: Option<String>,
    client_id: Option<String>,
}

/// Reads one HTTP request head and returns the request target of a GET.
async fn read_request_target(sock: &mut tokio::net::TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    let read = async {
        loop {
            let n = sock.read(&mut tmp).await.ok()?;
            if n == 0 {
                return None;
            }
            buf.extend_from_slice(&tmp[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") || buf.len() > 16 * 1024 {
                return Some(());
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), read).await.ok()??;
    let head = String::from_utf8_lossy(&buf);
    let mut parts = head.lines().next()?.split_whitespace();
    (parts.next()? == "GET").then_some(())?;
    parts.next().filter(|t| t.starts_with('/')).map(str::to_string)
}

async fn respond(sock: &mut tokio::net::TcpStream, status: &str, message: &str) {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Juttusihti</title></head>\
         <body style=\"font-family:sans-serif;margin:3em\"><p>{message}</p></body></html>"
    );
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = sock.write_all(head.as_bytes()).await;
    let _ = sock.write_all(body.as_bytes()).await;
    let _ = sock.shutdown().await;
}

/// Saves the rotated refresh token (`Some`) or clears it (`None`). Called
/// on a blocking thread.
pub type Persist = Arc<dyn Fn(Option<&str>) -> Result<(), String> + Send + Sync>;

struct Tokens {
    refresh: Option<String>,
    access: Option<(String, Instant)>,
}

/// A signed-in ChatGPT account: hands out fresh access tokens, refreshing
/// and persisting the rotating refresh token as needed.
pub struct Session {
    servers: Servers,
    client_id: String,
    http: reqwest::Client,
    /// Held across a refresh, so concurrent callers never race a rotating token.
    tokens: tokio::sync::Mutex<Tokens>,
    persist: Persist,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session").field("client_id", &self.client_id).finish_non_exhaustive()
    }
}

impl Session {
    /// Resumes a stored sign-in from its refresh token.
    pub fn resume(
        servers: Servers,
        client_id: String,
        refresh_token: String,
        persist: Persist,
    ) -> Result<Self, AuthError> {
        Self::build(servers, client_id, refresh_token, None, persist)
    }

    fn build(
        servers: Servers,
        client_id: String,
        refresh_token: String,
        access: Option<(String, Instant)>,
        persist: Persist,
    ) -> Result<Self, AuthError> {
        Ok(Self {
            servers,
            client_id,
            http: http_client()?,
            tokens: tokio::sync::Mutex::new(Tokens { refresh: Some(refresh_token), access }),
            persist,
        })
    }

    pub fn api_base(&self) -> &str {
        &self.servers.api_base
    }

    async fn store(&self, token: Option<String>) -> Result<(), String> {
        let persist = self.persist.clone();
        tokio::task::spawn_blocking(move || persist(token.as_deref())).await.map_err(|e| e.to_string())?
    }

    /// A valid access token, refreshing it first if it is (nearly) expired.
    pub async fn access_token(&self) -> Result<String, AuthError> {
        let mut t = self.tokens.lock().await;
        if let Some((tok, exp)) = &t.access
            && Instant::now() + EXPIRY_MARGIN < *exp
        {
            return Ok(tok.clone());
        }
        let refresh = t.refresh.clone().ok_or(AuthError::SignedOut)?;
        let body = form(&[
            ("grant_type", "refresh_token"),
            ("client_id", &self.client_id),
            ("refresh_token", &refresh),
            ("resource", API_BASE),
        ]);
        match token_request(&self.http, &self.servers, body).await {
            Ok(r) => {
                if !r.scope.as_deref().is_none_or(has_plan_scope) {
                    return Err(AuthError::NotEligible);
                }
                if let Some(new) = r.refresh_token {
                    // The old token is now spent: keep the new one in memory
                    // even if saving fails, and report the failure.
                    t.refresh = Some(new.clone());
                    if let Err(e) = self.store(Some(new)).await {
                        tracing::warn!(error = %e, "could not save the rotated ChatGPT refresh token");
                    }
                }
                let exp = Instant::now() + Duration::from_secs(r.expires_in.unwrap_or(3600));
                t.access = Some((r.access_token.clone(), exp));
                Ok(r.access_token)
            }
            Err(TokenError::Rejected { code, status }) if is_dead_refresh(&code) || status == 401 => {
                t.refresh = None;
                t.access = None;
                let _ = self.store(None).await;
                Err(AuthError::SignedOut)
            }
            Err(TokenError::Rejected { code, status }) => {
                Err(AuthError::Protocol(format!("token refresh failed ({status} {code})")))
            }
            // Temporary failures keep the credentials (per OpenAI's guidance).
            Err(TokenError::Network(m)) => Err(AuthError::Network(m)),
            Err(TokenError::Other(m)) => Err(AuthError::Protocol(m)),
        }
    }

    /// Drops the cached access token so the next call refreshes (after a 401).
    pub async fn expire_access_token(&self) {
        self.tokens.lock().await.access = None;
    }

    /// Revokes the refresh token at OpenAI (best effort) and clears it
    /// locally. The registration (issued client id) is kept for the next
    /// sign-in.
    pub async fn sign_out(&self) -> Result<(), AuthError> {
        let mut t = self.tokens.lock().await;
        let revoked = match t.refresh.clone() {
            Some(refresh) => revoke(&self.http, &self.servers, &self.client_id, &refresh).await,
            None => Ok(()),
        };
        t.refresh = None;
        t.access = None;
        self.store(None).await.map_err(AuthError::Store)?;
        revoked
    }

    /// Models the signed-in plan can use, as (id, display name).
    pub async fn models(&self) -> Result<Vec<(String, String)>, AuthError> {
        let token = self.access_token().await?;
        let resp = self
            .http
            .get(format!("{}/models", self.servers.api_base.trim_end_matches('/')))
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| AuthError::Network(e.to_string()))?;
        let status = resp.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            self.expire_access_token().await;
            return Err(AuthError::SignedOut);
        }
        let bytes = resp.bytes().await.map_err(|e| AuthError::Network(e.to_string()))?;
        if !status.is_success() {
            return Err(AuthError::Protocol(format!("model list returned {status}")));
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| AuthError::Protocol(e.to_string()))?;
        Ok(parse_models(&v))
    }
}

/// Listed models from `/v1/models` for a plan token (`models[]` with
/// `slug`/`display_name`/`visibility`), falling back to the API key shape.
fn parse_models(v: &Value) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = match v["models"].as_array() {
        Some(models) => models
            .iter()
            .filter(|m| m["visibility"] == "list")
            .filter_map(|m| {
                let slug = m["slug"].as_str()?.to_string();
                let name = m["display_name"].as_str().map_or_else(|| slug.clone(), str::to_string);
                Some((slug, name))
            })
            .collect(),
        None => v["data"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter_map(|m| m["id"].as_str().map(|id| (id.to_string(), id.to_string())))
            .collect(),
    };
    out.retain(|(slug, _)| crate::valid_model(slug));
    out
}

async fn revoke(http: &reqwest::Client, servers: &Servers, client_id: &str, refresh: &str) -> Result<(), AuthError> {
    let base = servers.auth_base.trim_end_matches('/');
    let bytes = http
        .get(format!("{base}/.well-known/openid-configuration"))
        .send()
        .await
        .map_err(|e| AuthError::Network(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| AuthError::Network(e.to_string()))?;
    let config: Value = serde_json::from_slice(&bytes).map_err(|e| AuthError::Protocol(e.to_string()))?;
    let endpoint = config["revocation_endpoint"]
        .as_str()
        .filter(|u| u.starts_with(base))
        .ok_or_else(|| AuthError::Protocol("no revocation endpoint".into()))?;
    let resp = http
        .post(endpoint)
        .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(form(&[("token", refresh), ("token_type_hint", "refresh_token"), ("client_id", client_id)]))
        .send()
        .await
        .map_err(|e| AuthError::Network(e.to_string()))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(AuthError::Protocol(format!("revocation returned {}", resp.status())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(claims: Value) -> String {
        format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()))
    }

    #[test]
    fn pkce_challenge_is_unpadded_base64url_sha256() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mJ92ZvqvKbHJUJYI-hPLDGeTXRtoNs"),
            // Computed independently with Python hashlib + base64.urlsafe_b64encode.
            "3woXZ3F49ILVZh6UgqZ6AoxzNeu8aSEhhAfg7qTW2hA"
        );
    }

    #[test]
    fn host_id_is_a_v4_uuid_urn() {
        let r = Registration::new();
        let id = r.host_id.strip_prefix("urn:uuid:").unwrap();
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.iter().map(|p| p.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12]);
        assert!(parts[2].starts_with('4'));
        assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
        assert_ne!(Registration::new().host_id, r.host_id);
    }

    #[test]
    fn id_token_claims_are_checked() {
        let good = json_claims("https://auth.openai.com", "oaiapp_1", "n1", 2_000);
        let id = check_id_token(&jwt(good.clone()), "https://auth.openai.com", "oaiapp_1", "n1", 1_000).unwrap();
        assert_eq!(id, Identity { subject: "user-1".into(), email: Some("a@example.com".into()) });

        let check = |claims: Value| check_id_token(&jwt(claims), "https://auth.openai.com", "oaiapp_1", "n1", 1_000);
        assert!(check(json_claims("https://evil.example", "oaiapp_1", "n1", 2_000)).is_err(), "issuer");
        assert!(check(json_claims("https://auth.openai.com", "other", "n1", 2_000)).is_err(), "audience");
        assert!(check(json_claims("https://auth.openai.com", "oaiapp_1", "n2", 2_000)).is_err(), "nonce");
        assert!(check(json_claims("https://auth.openai.com", "oaiapp_1", "n1", 999)).is_err(), "expired");
        let mut arr = good;
        arr["aud"] = serde_json::json!(["x", "oaiapp_1"]);
        assert!(check(arr).is_ok(), "array audience");
        assert!(check_id_token("garbage", "https://auth.openai.com", "oaiapp_1", "n1", 0).is_err());
    }

    fn json_claims(iss: &str, aud: &str, nonce: &str, exp: i64) -> Value {
        serde_json::json!({"iss": iss, "aud": aud, "nonce": nonce, "exp": exp, "sub": "user-1", "email": "a@example.com"})
    }

    #[test]
    fn plan_scope_must_be_granted() {
        assert!(has_plan_scope("openid offline_access chatgpt.tokens.use.direct"));
        assert!(!has_plan_scope("openid chatgpt.tokens.use.direct.extra"));
        assert!(!has_plan_scope(""));
    }

    #[test]
    fn model_list_keeps_listed_safe_models() {
        let v = serde_json::json!({"models": [
            {"slug": "gpt-a", "display_name": "GPT A", "visibility": "list"},
            {"slug": "gpt-hidden", "visibility": "hide"},
            {"slug": "../x", "visibility": "list"},
        ]});
        assert_eq!(parse_models(&v), vec![("gpt-a".to_string(), "GPT A".to_string())]);
    }
}
