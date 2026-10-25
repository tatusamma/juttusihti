//! Provider API keys in the operating system's secret store (Windows
//! Credential Manager, Secret Service on GNOME/KDE, macOS Keychain).
//! Keys are never written to the database or settings.

use juttusihti_core::pricing::Provider;

const SERVICE: &str = "fi.juttusihti.Juttusihti";

fn account(p: Provider) -> &'static str {
    match p {
        Provider::Anthropic => "anthropic-api-key",
        Provider::Openai => "openai-api-key",
        Provider::Gemini => "gemini-api-key",
    }
}

fn entry(p: Provider) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SERVICE, account(p)).map_err(|e| format!("secret store unavailable: {e}"))
}

pub fn get(p: Provider) -> Result<Option<String>, String> {
    match entry(p)?.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("could not read the API key: {e}")),
    }
}

pub fn set(p: Provider, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() || key.len() > 512 || key.chars().any(char::is_whitespace) {
        return Err("That does not look like an API key.".into());
    }
    entry(p)?.set_password(key).map_err(|e| format!("could not store the API key: {e}"))
}

pub fn delete(p: Provider) -> Result<(), String> {
    match entry(p)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("could not remove the API key: {e}")),
    }
}

/// Which providers have a stored key.
pub fn status() -> Vec<(Provider, Result<bool, String>)> {
    Provider::ALL.iter().map(|p| (*p, get(*p).map(|k| k.is_some()))).collect()
}
