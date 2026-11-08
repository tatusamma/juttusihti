//! AI provider clients (OpenAI, Anthropic, Gemini) and the runner that
//! executes a user-approved analysis session.
//!
//! All provider traffic goes through [`Client`], which speaks each
//! provider's HTTP API directly, requests JSON-schema-constrained output,
//! bounds response sizes and retries only transient failures. Model output
//! is untrusted: [`prompts`] validates every response before anything is
//! stored, and nothing in a response is ever executed.

pub mod chatgpt;
pub mod decisions;
pub mod factcheck;
pub mod prompts;
mod providers;
pub mod runner;
pub mod sources;

use std::sync::Arc;
use std::time::Duration;

use juttusihti_core::pricing::{Billing, Provider, Usage};
use reqwest::StatusCode;
use serde_json::Value;

pub use runner::{Approval, Control, Progress, RunError, RunSummary, Runner};

/// What to tell the user when the OpenAI API account is empty; callers add
/// what to do next.
pub(crate) const NO_CREDIT: &str = "Your OpenAI API account has no credit left. Add credit at https://platform.openai.com/settings/organization/billing/";

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
    /// Reasoning effort for OpenAI reasoning models ("low" keeps simple
    /// extraction fast and cheap). Dropped automatically if the model
    /// rejects it; ignored by the other providers.
    pub reasoning_effort: Option<&'static str>,
    /// Lets the model search the web (the provider's own search tool).
    pub web_search: Option<WebSearch>,
}

/// Provider web search for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSearch {
    /// Cap on searches where the provider supports one (Gemini has none).
    pub max_uses: u32,
}

/// What a reply says about the web searches the provider ran.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WebEvidence {
    pub searches: u32,
    /// Distinct http(s) sources, at most 50.
    pub sources: Vec<WebSource>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WebSource {
    pub url: String,
    pub title: String,
    /// The source's domain when the URL is a redirect (Gemini).
    pub site: Option<String>,
}

/// Whether a failed request means the model or credential cannot use the
/// web search tool, so the caller may retry without it.
pub fn is_tool_unsupported(e: &AiError) -> bool {
    match e {
        AiError::Unsupported(_) => true,
        AiError::Status { status: 400 | 404, message } => {
            let m = message.to_ascii_lowercase();
            // A rejected request field (e.g. max_tool_calls) is a request
            // shape bug, not a model that cannot search; do not hide it.
            if m.contains("unsupported parameter") {
                return false;
            }
            ["tool", "web_search", "google_search"].iter().any(|k| m.contains(k))
        }
        _ => false,
    }
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
    /// Empty for requests without search.
    pub web: WebEvidence,
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
        Ok(Self { http, endpoint, policy, no_reasoning_effort: Default::default() })
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
            Auth::ApiKey(k) => k.clone(),
            Auth::ChatGpt(s) => s.access_token().await.map_err(|e| (AiError::Auth(e), None))?,
        };
        let mut req = self.http.post(url).header(reqwest::header::CONTENT_TYPE, "application/json");
        for (k, v) in providers::auth_headers(self.endpoint.provider, &token) {
            req = req.header(k, v);
        }
        if let Some(t) = timeout {
            req = req.timeout(t);
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
