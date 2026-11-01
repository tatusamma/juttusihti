//! Provider credentials in the operating system's secret store (Windows
//! Credential Manager, Secret Service on GNOME/KDE, macOS Keychain).
//! Credentials are never written to the database or settings.

use juttusihti_ai::chatgpt::Registration;
use juttusihti_core::pricing::Provider;

const SERVICE: &str = "fi.juttusihti.Juttusihti";
/// Rotating refresh token of the ChatGPT sign-in. Access tokens are only
/// kept in memory.
const CHATGPT_TOKEN: &str = "openai-chatgpt-refresh-token";
/// Issued client id, host id and account of the ChatGPT sign-in (not
/// secret, but kept with the token it belongs to).
const CHATGPT_REGISTRATION: &str = "openai-chatgpt-registration";

fn account(p: Provider) -> &'static str {
    match p {
        Provider::Anthropic => "anthropic-api-key",
        Provider::Openai => "openai-api-key",
        Provider::Gemini => "gemini-api-key",
        // Optional: only for servers started with a key (`--api-key`).
        Provider::Local => "local-server-api-key",
    }
}

fn entry(account: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account).map_err(|e| format!("secret store unavailable: {e}"))
}

fn read(account: &str, what: &str) -> Result<Option<String>, String> {
    match entry(account)?.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("could not read the {what}: {e}")),
    }
}

fn remove(account: &str, what: &str) -> Result<(), String> {
    match entry(account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("could not remove the {what}: {e}")),
    }
}

pub fn get(p: Provider) -> Result<Option<String>, String> {
    read(account(p), "API key")
}

pub fn set(p: Provider, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() || key.len() > 512 || key.chars().any(char::is_whitespace) {
        return Err("That does not look like an API key.".into());
    }
    entry(account(p))?.set_password(key).map_err(|e| format!("could not store the API key: {e}"))
}

pub fn delete(p: Provider) -> Result<(), String> {
    remove(account(p), "API key")
}

/// Which providers have a stored key.
pub fn status() -> Vec<(Provider, Result<bool, String>)> {
    Provider::ALL.iter().map(|p| (*p, get(*p).map(|k| k.is_some()))).collect()
}

pub fn chatgpt_registration() -> Result<Option<Registration>, String> {
    read(CHATGPT_REGISTRATION, "ChatGPT sign-in")?
        .map(|s| serde_json::from_str(&s).map_err(|e| format!("stored ChatGPT sign-in is invalid: {e}")))
        .transpose()
}

pub fn set_chatgpt_registration(r: &Registration) -> Result<(), String> {
    let json = serde_json::to_string(r).map_err(|e| e.to_string())?;
    entry(CHATGPT_REGISTRATION)?.set_password(&json).map_err(|e| format!("could not store the ChatGPT sign-in: {e}"))
}

pub fn chatgpt_refresh_token() -> Result<Option<String>, String> {
    read(CHATGPT_TOKEN, "ChatGPT sign-in")
}

/// Saves (`Some`) or removes (`None`) the ChatGPT refresh token.
pub fn set_chatgpt_refresh_token(token: Option<&str>) -> Result<(), String> {
    match token {
        Some(t) => {
            entry(CHATGPT_TOKEN)?.set_password(t).map_err(|e| format!("could not store the ChatGPT sign-in: {e}"))
        }
        None => remove(CHATGPT_TOKEN, "ChatGPT sign-in"),
    }
}
