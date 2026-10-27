//! The app-wide ChatGPT sign-in (Sign in with ChatGPT): one session shared
//! by every run, so the rotating refresh token is only ever used by one
//! holder at a time.

use std::sync::{Arc, Mutex};

use juttusihti_ai::chatgpt::{PendingSignIn, Persist, Registration, Servers, Session, SignedIn};

use crate::secrets;

static SESSION: Mutex<Option<Arc<Session>>> = Mutex::new(None);

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(f).await.expect("secret store task panicked")
}

fn persist() -> Persist {
    Arc::new(secrets::set_chatgpt_refresh_token)
}

fn cached() -> Option<Arc<Session>> {
    SESSION.lock().expect("session lock").clone()
}

fn set_cached(s: Option<Arc<Session>>) {
    *SESSION.lock().expect("session lock") = s;
}

/// Signed-in account, if any: `Some(email or "ChatGPT account")`.
pub async fn status() -> Result<Option<String>, String> {
    blocking(|| {
        let token = secrets::chatgpt_refresh_token()?;
        let reg = secrets::chatgpt_registration()?;
        Ok(match (token, reg) {
            (Some(_), Some(r)) if r.client_id.is_some() => Some(r.email.unwrap_or_else(|| "ChatGPT account".into())),
            _ => None,
        })
    })
    .await
}

/// The signed-in session, resumed from the secret store when needed.
pub async fn session() -> Result<Arc<Session>, String> {
    if let Some(s) = cached() {
        return Ok(s);
    }
    let (reg, token) =
        blocking(|| Ok::<_, String>((secrets::chatgpt_registration()?, secrets::chatgpt_refresh_token()?))).await?;
    let (Some(client_id), Some(token)) = (reg.and_then(|r| r.client_id), token) else {
        return Err("Not signed in with ChatGPT. Sign in under AI setup.".into());
    };
    let s = Arc::new(Session::resume(Servers::default(), client_id, token, persist()).map_err(|e| e.to_string())?);
    // Another task may have resumed meanwhile; keep a single session.
    let mut slot = SESSION.lock().expect("session lock");
    Ok(slot.get_or_insert(s).clone())
}

/// A sign-in in progress, shared with the UI while the browser is open.
#[derive(Clone)]
pub struct Pending(Arc<tokio::sync::Mutex<Option<PendingSignIn>>>);

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Pending")
    }
}

/// Starts the loopback listener and returns the address to open.
pub async fn start_sign_in() -> Result<(String, Pending), String> {
    let reg = blocking(|| -> Result<Registration, String> {
        match secrets::chatgpt_registration()? {
            Some(r) => Ok(r),
            None => {
                // Persist the host id first so it stays stable across attempts.
                let r = Registration::new();
                secrets::set_chatgpt_registration(&r)?;
                Ok(r)
            }
        }
    })
    .await?;
    let pending = PendingSignIn::start(Servers::default(), reg).await.map_err(|e| e.to_string())?;
    let url = pending.url().to_string();
    Ok((url, Pending(Arc::new(tokio::sync::Mutex::new(Some(pending))))))
}

/// Waits for the browser, then stores the sign-in. Returns the account.
pub async fn finish_sign_in(p: Pending) -> Result<String, String> {
    let pending = p.0.lock().await.take().ok_or("This sign-in already finished.")?;
    let signed: SignedIn = pending.finish().await.map_err(|e| e.to_string())?;
    let (reg, token) = (signed.registration.clone(), signed.refresh_token.clone());
    blocking(move || {
        secrets::set_chatgpt_registration(&reg)?;
        secrets::set_chatgpt_refresh_token(Some(&token))
    })
    .await?;
    let email = signed.registration.email.clone().unwrap_or_else(|| "ChatGPT account".into());
    let session = signed.into_session(Servers::default(), persist()).map_err(|e| e.to_string())?;
    set_cached(Some(Arc::new(session)));
    Ok(email)
}

/// Revokes the sign-in at OpenAI (best effort) and forgets it locally. The
/// registration is kept so the next sign-in reuses the issued client id.
pub async fn sign_out() -> Result<(), String> {
    let session = match cached() {
        Some(s) => Some(s),
        None => session().await.ok(),
    };
    set_cached(None);
    let revoked = match session {
        Some(s) => s.sign_out().await.map_err(|e| e.to_string()),
        None => Ok(()),
    };
    blocking(|| secrets::set_chatgpt_refresh_token(None)).await?;
    revoked.map_err(|e| format!("Signed out here, but OpenAI could not be told: {e}"))
}

/// Models the signed-in plan can use.
pub async fn models() -> Result<Vec<String>, String> {
    let s = session().await?;
    let models = s.models().await.map_err(|e| e.to_string())?;
    Ok(models.into_iter().map(|(id, _)| id).collect())
}
