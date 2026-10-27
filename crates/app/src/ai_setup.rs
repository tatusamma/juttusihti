//! The AI defaults in effect: saved ones, or detected from what is set up
//! (a ChatGPT sign-in first, then any stored API key).

use juttusihti_core::ai_defaults::AiDefaults;
use juttusihti_core::pricing::{Billing, Provider};

use crate::db::Db;
use crate::{plan, secrets};

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("secret store task panicked")
}

/// Saved defaults; until the user saves any, the provider follows what is
/// connected.
pub async fn defaults(db: &Db) -> Result<AiDefaults, String> {
    let (saved, explicit) = db
        .call(|s| -> juttusihti_core::Result<_> { Ok((s.ai_defaults()?, s.setting("ai_defaults")?.is_some())) })
        .await?;
    if explicit {
        return Ok(saved);
    }
    if matches!(plan::status().await, Ok(Some(_))) {
        return Ok(AiDefaults { provider: Provider::Openai, billing: Billing::ChatGptPlan, ..saved });
    }
    let keys = blocking(secrets::status).await;
    let with_key = [Provider::Openai, Provider::Anthropic, Provider::Gemini]
        .into_iter()
        .find(|p| keys.iter().any(|(k, s)| k == p && matches!(s, Ok(true))));
    Ok(match with_key {
        Some(provider) => AiDefaults { provider, billing: Billing::ApiKey, ..saved },
        None => saved,
    })
}

pub async fn save(db: &Db, d: AiDefaults) -> Result<(), String> {
    db.call(move |s| s.set_ai_defaults(&d)).await
}

/// Whether the credentials for `d` are in place; the error says what to do.
pub async fn readiness(d: &AiDefaults) -> Result<(), String> {
    let provider = d.provider;
    match d.billing() {
        Billing::ChatGptPlan => match plan::status().await? {
            Some(_) => Ok(()),
            None => Err("Sign in with ChatGPT under AI setup first.".into()),
        },
        Billing::ApiKey => match blocking(move || secrets::get(provider)).await? {
            Some(_) => Ok(()),
            None => Err(format!("Add a {provider} API key under AI setup first.")),
        },
    }
}

/// "gpt-6-luna on your ChatGPT plan" / "claude-haiku-5-5 (API key)".
pub fn describe(d: &AiDefaults) -> String {
    match d.billing() {
        Billing::ChatGptPlan => format!("{} on your ChatGPT plan", d.model()),
        Billing::ApiKey => format!("{} · {}", d.provider, d.model()),
    }
}
