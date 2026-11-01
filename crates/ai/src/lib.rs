//! AI provider clients (OpenAI, Anthropic, Gemini, and local or self-hosted
//! OpenAI-compatible servers) and the runner that executes a user-approved
//! analysis session.
//!
//! All provider traffic goes through [`Client`], which speaks each
//! provider's HTTP API directly, requests JSON-schema-constrained output,
//! bounds response sizes and retries only transient failures. A local server
//! that cannot constrain output to a schema is asked for JSON in the prompt
//! instead. Model output
//! is untrusted: [`prompts`] validates every response before anything is
//! stored, and nothing in a response is ever executed.

pub mod chatgpt;
pub mod decisions;
pub mod prompts;
mod providers;
pub mod runner;

use std::sync::Arc;
use std::time::Duration;

use juttusihti_core::pricing::{Billing, Provider, Usage};
use reqwest::StatusCode;
use serde_json::Value;

pub use runner::{Approval, Control, Progress, RunError, RunSummary, Runner};

/// Upper bound for a provider response body.
pub const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

/// How requests are authorized.
#[derive(Clone)]
pub enum Auth {
    ApiKey(String),
    /// Sign in with ChatGPT: OpenAI requests charged to the user's plan.
    ChatGpt(Arc<chatgpt::Session>),
    /// No credentials: a local server that does not check keys.
    None,
}

/// Where and how to reach a provider. Credentials are never logged.
#[derive(Clone)]
pub struct Endpoint {
    pub provider: Provider,
    pub base_url: String,
    pub auth: Auth,
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("billing", &self.billing())
            .finish_non_exhaustive()
    }
}

/// Hosted providers' public APIs. A local server has none: its address is
/// always the user's.
fn official_base(provider: Provider) -> Option<&'static str> {
    match provider {
        Provider::Anthropic => Some("https://api.anthropic.com"),
        Provider::Openai => Some("https://api.openai.com"),
        Provider::Gemini => Some("https://generativelanguage.googleapis.com"),
        Provider::Local => None,
    }
}

/// Checks and normalizes a local server address to the base that
/// `/chat/completions` and `/models` are appended to.
///
/// Accepts what servers print on start-up or document: `http://host:8080/v1`,
/// `http://host:11434` (no path → `/v1`), or a full
/// `…/v1/chat/completions` URL. Credentials, queries and fragments are
/// refused so a key never ends up in an address.
pub fn normalize_local_base(input: &str) -> Result<String, String> {
    let example = "e.g. http://192.168.1.20:8080/v1";
    let t = input.trim();
    if t.is_empty() {
        return Err(format!("Enter the server address, {example}."));
    }
    let mut url = url::Url::parse(t).map_err(|_| format!("That is not a valid address ({example})."))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("The address must start with http:// or https:// ({example})."));
    }
    if url.host_str().is_none_or(str::is_empty) {
        return Err(format!("The address has no host ({example})."));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("Put the API key in its own field, not in the address.".into());
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err("The address cannot contain ? or #.".into());
    }
    let mut path = url.path().trim_end_matches('/').to_string();
    for suffix in ["/chat/completions", "/models"] {
        if let Some(p) = path.strip_suffix(suffix) {
            path = p.trim_end_matches('/').to_string();
            break;
        }
    }
    if path.is_empty() {
        path = "/v1".into();
    }
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_string())
}

impl Endpoint {
    /// The provider's public API with an API key. A local server has no
    /// public API: use [`Endpoint::local`].
    pub fn official(provider: Provider, api_key: String) -> Self {
        Self { provider, base_url: official_base(provider).unwrap_or_default().into(), auth: Auth::ApiKey(api_key) }
    }

    /// A local or self-hosted OpenAI-compatible server at `base_url` (see
    /// [`normalize_local_base`]), with an optional API key.
    pub fn local(base_url: &str, api_key: Option<String>) -> Result<Self, AiError> {
        let base_url = normalize_local_base(base_url).map_err(AiError::BadAddress)?;
        let auth = match api_key.map(|k| k.trim().to_string()) {
            Some(k) if !k.is_empty() => Auth::ApiKey(k),
            _ => Auth::None,
        };
        Ok(Self { provider: Provider::Local, base_url, auth })
    }

    /// OpenAI's public API, charged to the signed-in ChatGPT plan.
    pub fn chatgpt(session: Arc<chatgpt::Session>) -> Self {
        let base_url = session.api_base().trim_end_matches('/').trim_end_matches("/v1").to_string();
        Self { provider: Provider::Openai, base_url, auth: Auth::ChatGpt(session) }
    }

    pub fn billing(&self) -> Billing {
        match self.auth {
            Auth::ApiKey(_) | Auth::None => Billing::ApiKey,
            Auth::ChatGpt(_) => Billing::ChatGptPlan,
        }
    }
}

/// One structured-output request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub model: String,
    pub system: String,
    pub user: String,
    pub schema_name: String,
    pub schema: Value,
    pub max_output_tokens: u32,
    /// Reasoning effort for OpenAI reasoning models ("low" keeps simple
    /// extraction fast and cheap). Dropped automatically if the model
    /// rejects it; ignored by the other providers.
    pub reasoning_effort: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Parsed JSON text from the model (not yet validated against the task).
    Json(Value),
    /// The provider declined (safety classifier, content filter, block).
    Refused(String),
    /// Output hit the token limit; content is incomplete.
    Truncated,
    /// Text that is not JSON despite the schema.
    NotJson(String),
}

/// A provider reply with billed usage.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub outcome: Outcome,
    /// Billing categories; `output` includes reasoning tokens.
    pub usage: Usage,
    pub reasoning_tokens: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    /// The request never reached the provider (no charge possible).
    #[error("could not reach provider: {0}")]
    NotSent(String),
    /// The request may have been processed but the outcome is unknown
    /// (timeout, connection dropped mid-response). Treat as possibly charged.
    #[error("provider did not answer in time: {0}")]
    Unknown(String),
    #[error("provider returned {status}: {message}")]
    Status { status: u16, message: String },
    #[error("unexpected provider response: {0}")]
    Malformed(String),
    #[error("provider response exceeded {MAX_RESPONSE_BYTES} bytes")]
    TooLarge,
    #[error("invalid model id")]
    InvalidModel,
    /// The local server address is not usable; nothing was sent.
    #[error("{0}")]
    BadAddress(String),
    /// Credentials could not be obtained (ChatGPT sign-in); nothing was sent.
    #[error("{0}")]
    Auth(#[from] chatgpt::AuthError),
    /// The ChatGPT plan (or this app's weekly cap) is used up for now.
    #[error("ChatGPT plan usage limit reached: {0}")]
    PlanLimit(String),
    /// The OpenAI API account has no credit left. Retrying cannot help.
    #[error("the OpenAI API account has no credit left: {0}")]
    NoCredit(String),
    /// The feature is not available with this kind of credential.
    #[error("{0}")]
    Unsupported(&'static str),
}

impl AiError {
    /// Whether the provider may have billed this request.
    pub fn possibly_charged(&self) -> bool {
        matches!(self, AiError::Unknown(_) | AiError::Malformed(_) | AiError::TooLarge)
    }

    fn retryable(&self) -> bool {
        match self {
            AiError::NotSent(_) => true,
            AiError::Auth(chatgpt::AuthError::Network(_)) => true,
            AiError::Status { status, .. } => matches!(status, 408 | 409 | 429 | 500 | 502 | 503 | 504 | 529),
            _ => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub timeout: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_backoff: Duration::from_secs(2),
            max_backoff: Duration::from_secs(60),
            timeout: Duration::from_secs(300),
        }
    }
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    endpoint: Endpoint,
    policy: RetryPolicy,
    /// Set once the model rejected a reasoning effort, so later requests
    /// do not repeat the failed attempt.
    no_reasoning_effort: Arc<std::sync::atomic::AtomicBool>,
    /// Set once a local server worked only without a JSON schema, so later
    /// requests ask for JSON in the prompt straight away.
    plain_json: Arc<std::sync::atomic::AtomicBool>,
}

/// Model ids go into URL paths (Gemini); allow only safe characters.
pub(crate) fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 128
        && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
}

/// Local model names only go into the JSON body, and servers name them
/// like `llama3.1:8b`, `Qwen/Qwen3-8B` or `model.gguf`: allow those, but no
/// whitespace or control characters.
pub(crate) fn valid_model_for(provider: Provider, model: &str) -> bool {
    match provider {
        Provider::Local => {
            !model.is_empty()
                && model.len() <= 256
                && model
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '/' | '@' | '+'))
        }
        _ => valid_model(model),
    }
}

/// Whether `model` is usable as a local server's model name.
pub fn valid_local_model(model: &str) -> bool {
    valid_model_for(Provider::Local, model)
}

/// Whether a local server's error may mean it cannot constrain output to a
/// JSON schema (unknown `response_format`, schema-to-grammar failure, no
/// guided-decoding backend). Auth, missing-route, rate and timeout errors
/// are not: retrying without the schema would not help.
fn may_lack_schema_support(e: &AiError) -> bool {
    matches!(e, AiError::Status { status, .. } if matches!(status, 400 | 415 | 422 | 500 | 501))
}

impl Client {
    pub fn new(endpoint: Endpoint, policy: RetryPolicy) -> Result<Self, AiError> {
        let http = reqwest::Client::builder()
            .user_agent(concat!("Juttusihti/", env!("CARGO_PKG_VERSION")))
            .timeout(policy.timeout)
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| AiError::NotSent(e.to_string()))?;
        if endpoint.provider == Provider::Local {
            normalize_local_base(&endpoint.base_url).map_err(AiError::BadAddress)?;
        }
        Ok(Self { http, endpoint, policy, no_reasoning_effort: Default::default(), plain_json: Default::default() })
    }

    pub fn provider(&self) -> Provider {
        self.endpoint.provider
    }

    pub fn billing(&self) -> Billing {
        self.endpoint.billing()
    }

    /// Sends a structured-output request, retrying transient failures.
    pub async fn complete(&self, req: &Request) -> Result<Reply, AiError> {
        if !valid_model_for(self.endpoint.provider, &req.model) {
            return Err(AiError::InvalidModel);
        }
        if self.endpoint.provider == Provider::Local {
            return self.complete_local(req).await;
        }
        use std::sync::atomic::Ordering;
        let (url, mut body) = providers::completion(&self.endpoint, req);
        if self.no_reasoning_effort.load(Ordering::Relaxed)
            && let Some(o) = body.as_object_mut()
        {
            o.remove("reasoning");
        }
        let json = match self.post_with_retries(&url, &body, None).await {
            // Not every model accepts every effort level; a rejected request is not billed.
            Err(AiError::Status { status: 400, message })
                if body.get("reasoning").is_some() && message.to_ascii_lowercase().contains("reasoning") =>
            {
                tracing::info!(model = %req.model, %message, "model rejected the reasoning effort; retrying without it");
                self.no_reasoning_effort.store(true, Ordering::Relaxed);
                if let Some(o) = body.as_object_mut() {
                    o.remove("reasoning");
                }
                self.post_with_retries(&url, &body, None).await?
            }
            other => other?,
        };
        providers::parse_completion(self.endpoint.provider, &json)
    }

    /// A local server: JSON-schema output first; if the server cannot do
    /// that, plain JSON prompting. Either way the caller validates the reply.
    async fn complete_local(&self, req: &Request) -> Result<Reply, AiError> {
        use std::sync::atomic::Ordering;
        let url = providers::local_url(&self.endpoint);
        if !self.plain_json.load(Ordering::Relaxed) {
            match self.post_with_retries(&url, &providers::local_body(req, true), None).await {
                Ok(json) => return providers::parse_completion(Provider::Local, &json),
                Err(e) if may_lack_schema_support(&e) => {
                    tracing::info!(model = %req.model, error = %e, "local server refused JSON-schema output; asking for JSON in the prompt");
                }
                Err(e) => return Err(e),
            }
            let json = self.post_with_retries(&url, &providers::local_body(req, false), None).await?;
            // Only a server that works without the schema keeps working that way.
            self.plain_json.store(true, Ordering::Relaxed);
            return providers::parse_completion(Provider::Local, &json);
        }
        let json = self.post_with_retries(&url, &providers::local_body(req, false), None).await?;
        providers::parse_completion(Provider::Local, &json)
    }

    /// Whether this client asks a local server for JSON in the prompt
    /// because the server could not use a JSON schema.
    pub fn uses_plain_json(&self) -> bool {
        self.plain_json.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Model ids a local server offers (`GET {base}/models`). Sends no
    /// article content, so it needs no consent.
    pub async fn list_models(&self) -> Result<Vec<String>, AiError> {
        if self.endpoint.provider != Provider::Local {
            return Err(AiError::Unsupported("listing models is only available for a local server"));
        }
        let url = format!("{}/models", self.endpoint.base_url.trim_end_matches('/'));
        let mut req = self.http.get(&url);
        if let Auth::ApiKey(k) = &self.endpoint.auth {
            for (h, v) in providers::auth_headers(Provider::Local, k) {
                req = req.header(h, v);
            }
        }
        let resp = req.timeout(Duration::from_secs(20)).send().await.map_err(|e| {
            if e.is_connect() || e.is_builder() {
                AiError::NotSent(e.to_string())
            } else {
                AiError::Unknown(e.to_string())
            }
        })?;
        let (status, bytes, _) = read_bounded(resp).await?;
        if status != StatusCode::OK {
            return Err(AiError::Status { status: status.as_u16(), message: providers::error_message(&bytes) });
        }
        let v: Value = serde_json::from_slice(&bytes).map_err(|e| AiError::Malformed(e.to_string()))?;
        providers::parse_models(&v)
    }

    /// Counts input tokens for a request with the provider's own counter.
    /// This sends the request content to the provider: only call it after
    /// the user consented to that.
    pub async fn count_tokens(&self, req: &Request) -> Result<u64, AiError> {
        if !valid_model_for(self.endpoint.provider, &req.model) {
            return Err(AiError::InvalidModel);
        }
        if self.billing() == Billing::ChatGptPlan {
            return Err(AiError::Unsupported("token counting is not available with a ChatGPT plan"));
        }
        let Some((url, body)) = providers::count_tokens(&self.endpoint, req) else {
            return Err(AiError::Unsupported("OpenAI-compatible servers have no standard token counter"));
        };
        let json = self.post_with_retries(&url, &body, None).await?;
        providers::parse_count(self.endpoint.provider, &json)
    }

    /// Sends a Decisions API request ([`decisions::request`]), retrying
    /// transient failures. Needs OpenAI with an API key.
    pub async fn decide(&self, body: &Value, timeout: Duration) -> Result<Value, AiError> {
        if !body["model"].as_str().is_some_and(valid_model) {
            return Err(AiError::InvalidModel);
        }
        if self.endpoint.provider != Provider::Openai || self.billing() != Billing::ApiKey {
            return Err(AiError::Unsupported("the Decisions API needs an OpenAI API key"));
        }
        let url = format!("{}/v1/decisions", self.endpoint.base_url.trim_end_matches('/'));
        self.post_with_retries(&url, body, Some(timeout)).await
    }

    /// `timeout` overrides the client's for this request.
    async fn post_with_retries(&self, url: &str, body: &Value, timeout: Option<Duration>) -> Result<Value, AiError> {
        let mut attempt = 0;
        let mut reauthorized = false;
        loop {
            attempt += 1;
            match self.post_once(url, body, timeout).await {
                Ok(v) => return Ok(v),
                // A plan access token can be revoked or expire early: refresh once.
                Err((AiError::Status { status: 401, .. }, _))
                    if !reauthorized && matches!(self.endpoint.auth, Auth::ChatGpt(_)) =>
                {
                    if let Auth::ChatGpt(s) = &self.endpoint.auth {
                        s.expire_access_token().await;
                    }
                    reauthorized = true;
                    attempt -= 1;
                }
                Err((e, retry_after)) if e.retryable() && attempt < self.policy.max_attempts => {
                    let backoff = self.policy.initial_backoff * 2u32.pow(attempt - 1);
                    let wait = retry_after.unwrap_or(backoff).min(self.policy.max_backoff);
                    tracing::debug!(attempt, ?wait, error = %e, "retrying provider request");
                    tokio::time::sleep(wait).await;
                }
                Err((e, _)) => return Err(e),
            }
        }
    }

    async fn post_once(
        &self,
        url: &str,
        body: &Value,
        timeout: Option<Duration>,
    ) -> Result<Value, (AiError, Option<Duration>)> {
        let token = match &self.endpoint.auth {
            Auth::ApiKey(k) => Some(k.clone()),
            Auth::ChatGpt(s) => Some(s.access_token().await.map_err(|e| (AiError::Auth(e), None))?),
            Auth::None => None,
        };
        let mut req = self.http.post(url).header(reqwest::header::CONTENT_TYPE, "application/json");
        for (k, v) in token.map(|t| providers::auth_headers(self.endpoint.provider, &t)).unwrap_or_default() {
            req = req.header(k, v);
        }
        if let Some(t) = timeout {
            req = req.timeout(t);
        }
        let resp = req.body(body.to_string()).send().await.map_err(|e| {
            let err = if e.is_connect() || e.is_builder() {
                AiError::NotSent(e.to_string())
            } else {
                AiError::Unknown(e.to_string())
            };
            (err, None)
        })?;
        let (status, bytes, retry_after) = read_bounded(resp).await.map_err(|e| (e, None))?;
        if status != StatusCode::OK {
            if providers::is_plan_limit(&bytes) {
                return Err((AiError::PlanLimit(providers::error_message(&bytes)), None));
            }
            if self.endpoint.provider == Provider::Openai
                && status == StatusCode::TOO_MANY_REQUESTS
                && providers::is_no_credit(&bytes)
            {
                return Err((AiError::NoCredit(providers::error_message(&bytes)), None));
            }
            return Err((
                AiError::Status { status: status.as_u16(), message: providers::error_message(&bytes) },
                retry_after,
            ));
        }
        if body["stream"] == true {
            return providers::final_response_from_stream(&bytes).map_err(|e| (e, None));
        }
        serde_json::from_slice(&bytes).map_err(|e| (AiError::Malformed(e.to_string()), None))
    }
}

/// Reads a response body up to [`MAX_RESPONSE_BYTES`], with its status and
/// any `Retry-After` delay.
async fn read_bounded(mut resp: reqwest::Response) -> Result<(StatusCode, Vec<u8>, Option<Duration>), AiError> {
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let mut bytes = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| AiError::Unknown(e.to_string()))? {
        if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(AiError::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, bytes, retry_after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_are_restricted() {
        assert!(valid_model("claude-haiku-5-5"));
        assert!(valid_model("gemini-3.8-flash"));
        assert!(!valid_model("../../v1/files"));
        assert!(!valid_model("model:generateContent?x"));
        assert!(!valid_model(""));
    }

    #[test]
    fn local_model_names_allow_server_conventions() {
        for m in ["llama3.1:8b", "Qwen/Qwen3-8B-Instruct", "gemma-3-12b-it-Q4_K_M.gguf", "org/model@rev"] {
            assert!(valid_model_for(Provider::Local, m), "{m}");
            assert!(!valid_model_for(Provider::Gemini, m) || !m.contains([':', '/', '@']), "{m}");
        }
        assert!(!valid_model_for(Provider::Local, ""));
        assert!(!valid_model_for(Provider::Local, "a b"));
        assert!(!valid_model_for(Provider::Local, "a\nb"));
        assert!(!valid_model_for(Provider::Local, &"x".repeat(257)));
    }

    #[test]
    fn local_addresses_are_normalized() {
        let n = |s: &str| normalize_local_base(s);
        assert_eq!(n("http://192.168.1.20:8080/v1").unwrap(), "http://192.168.1.20:8080/v1");
        assert_eq!(n(" http://192.168.1.20:8080/v1/ ").unwrap(), "http://192.168.1.20:8080/v1");
        assert_eq!(n("http://localhost:11434").unwrap(), "http://localhost:11434/v1", "no path → /v1");
        assert_eq!(n("http://localhost:1234/v1/chat/completions").unwrap(), "http://localhost:1234/v1");
        assert_eq!(n("http://localhost:8000/v1/models").unwrap(), "http://localhost:8000/v1");
        assert_eq!(n("https://gpu.example/openai/v1").unwrap(), "https://gpu.example/openai/v1");
        assert_eq!(n("http://[::1]:8080/v1").unwrap(), "http://[::1]:8080/v1");
        for bad in ["", "192.168.1.20:8080", "ftp://h/v1", "http://user:pw@h/v1", "http://h/v1?key=x", "http://h/v1#x"]
        {
            assert!(n(bad).is_err(), "{bad}");
        }
        let e = Endpoint::local("http://h:8080", Some("  ".into())).unwrap();
        assert!(matches!(e.auth, Auth::None), "a blank key means no key");
        assert_eq!(e.base_url, "http://h:8080/v1");
        assert_eq!(e.billing(), Billing::ApiKey);
        assert!(Endpoint::local("nope", None).is_err());
        let bad = Endpoint { provider: Provider::Local, base_url: String::new(), auth: Auth::None };
        assert!(matches!(Client::new(bad, RetryPolicy::default()), Err(AiError::BadAddress(_))));
    }

    #[test]
    fn endpoint_debug_hides_key() {
        let e = Endpoint::official(Provider::Openai, "sk-secret".into());
        assert!(!format!("{e:?}").contains("sk-secret"));
    }
}
