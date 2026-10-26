//! AI provider clients (OpenAI, Anthropic, Gemini) and the runner that
//! executes a user-approved analysis session.
//!
//! All provider traffic goes through [`Client`], which speaks each
//! provider's HTTP API directly, requests JSON-schema-constrained output,
//! bounds response sizes and retries only transient failures. Model output
//! is untrusted: [`prompts`] validates every response before anything is
//! stored, and nothing in a response is ever executed.

pub mod chatgpt;
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

fn official_base(provider: Provider) -> &'static str {
    match provider {
        Provider::Anthropic => "https://api.anthropic.com",
        Provider::Openai => "https://api.openai.com",
        Provider::Gemini => "https://generativelanguage.googleapis.com",
    }
}

impl Endpoint {
    /// The provider's public API with an API key.
    pub fn official(provider: Provider, api_key: String) -> Self {
        Self { provider, base_url: official_base(provider).into(), auth: Auth::ApiKey(api_key) }
    }

    /// OpenAI's public API, charged to the signed-in ChatGPT plan.
    pub fn chatgpt(session: Arc<chatgpt::Session>) -> Self {
        let base_url = session.api_base().trim_end_matches('/').trim_end_matches("/v1").to_string();
        Self { provider: Provider::Openai, base_url, auth: Auth::ChatGpt(session) }
    }

    pub fn billing(&self) -> Billing {
        match self.auth {
            Auth::ApiKey(_) => Billing::ApiKey,
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
    /// Credentials could not be obtained (ChatGPT sign-in); nothing was sent.
    #[error("{0}")]
    Auth(#[from] chatgpt::AuthError),
    /// The ChatGPT plan (or this app's weekly cap) is used up for now.
    #[error("ChatGPT plan usage limit reached: {0}")]
    PlanLimit(String),
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
}

/// Model ids go into URL paths (Gemini); allow only safe characters.
pub(crate) fn valid_model(model: &str) -> bool {
    !model.is_empty()
        && model.len() <= 128
        && model.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_'))
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
        Ok(Self { http, endpoint, policy })
    }

    pub fn provider(&self) -> Provider {
        self.endpoint.provider
    }

    pub fn billing(&self) -> Billing {
        self.endpoint.billing()
    }

    /// Sends a structured-output request, retrying transient failures.
    pub async fn complete(&self, req: &Request) -> Result<Reply, AiError> {
        if !valid_model(&req.model) {
            return Err(AiError::InvalidModel);
        }
        let (url, body) = providers::completion(&self.endpoint, req);
        let json = self.post_with_retries(&url, &body).await?;
        providers::parse_completion(self.endpoint.provider, &json)
    }

    /// Counts input tokens for a request with the provider's own counter.
    /// This sends the request content to the provider: only call it after
    /// the user consented to that.
    pub async fn count_tokens(&self, req: &Request) -> Result<u64, AiError> {
        if !valid_model(&req.model) {
            return Err(AiError::InvalidModel);
        }
        if self.billing() == Billing::ChatGptPlan {
            return Err(AiError::Unsupported("token counting is not available with a ChatGPT plan"));
        }
        let (url, body) = providers::count_tokens(&self.endpoint, req);
        let json = self.post_with_retries(&url, &body).await?;
        providers::parse_count(self.endpoint.provider, &json)
    }

    async fn post_with_retries(&self, url: &str, body: &Value) -> Result<Value, AiError> {
        let mut attempt = 0;
        let mut reauthorized = false;
        loop {
            attempt += 1;
            match self.post_once(url, body).await {
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

    async fn post_once(&self, url: &str, body: &Value) -> Result<Value, (AiError, Option<Duration>)> {
        let token = match &self.endpoint.auth {
            Auth::ApiKey(k) => k.clone(),
            Auth::ChatGpt(s) => s.access_token().await.map_err(|e| (AiError::Auth(e), None))?,
        };
        let mut req = self.http.post(url).header(reqwest::header::CONTENT_TYPE, "application/json");
        for (k, v) in providers::auth_headers(self.endpoint.provider, &token) {
            req = req.header(k, v);
        }
        let mut resp = req.body(body.to_string()).send().await.map_err(|e| {
            let err = if e.is_connect() || e.is_builder() {
                AiError::NotSent(e.to_string())
            } else {
                AiError::Unknown(e.to_string())
            };
            (err, None)
        })?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|e| (AiError::Unknown(e.to_string()), None))? {
            if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err((AiError::TooLarge, None));
            }
            bytes.extend_from_slice(&chunk);
        }
        if status != StatusCode::OK {
            if providers::is_plan_limit(&bytes) {
                return Err((AiError::PlanLimit(providers::error_message(&bytes)), None));
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
    fn endpoint_debug_hides_key() {
        let e = Endpoint::official(Provider::Openai, "sk-secret".into());
        assert!(!format!("{e:?}").contains("sk-secret"));
    }
}
