//! Async access to the SQLite store from the UI.
//!
//! The store owns a single read-write connection; all calls run on Tokio's
//! blocking pool behind a mutex so the UI thread never waits on SQLite.

use std::sync::{Arc, Mutex};

use juttusihti_core::store::Store;

#[derive(Clone)]
pub struct Db(Arc<Mutex<Store>>);

impl Db {
    pub fn new(store: Store) -> Self {
        Self(Arc::new(Mutex::new(store)))
    }

    /// The shared store, for long-running workers such as the analysis runner.
    pub fn shared(&self) -> Arc<Mutex<Store>> {
        self.0.clone()
    }

    /// Runs `f` with exclusive access to the store on a blocking thread.
    /// Errors are flattened to strings for display.
    pub async fn call<T, E>(&self, f: impl FnOnce(&mut Store) -> Result<T, E> + Send + 'static) -> Result<T, String>
    where
        T: Send + 'static,
        E: std::fmt::Display,
    {
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut store = inner.lock().map_err(|_| "database lock poisoned".to_string())?;
            f(&mut store).map_err(|e| e.to_string())
        })
        .await
        .map_err(|e| e.to_string())?
    }
}
