//! A single bounded background thread that owns the DuckDB snapshot, so
//! analytical queries never block the UI or hold SQLite locks.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;

use crate::{Analytics, Error, Limits, Result, SnapshotStats};

type Job = Box<dyn FnOnce(&mut Option<Analytics>) + Send>;

/// Queue depth; further requests get [`Error::Busy`] instead of piling up.
const QUEUE: usize = 4;

/// Cancels whatever query the worker is currently running.
#[derive(Clone, Default)]
pub struct CancelHandle(Arc<Mutex<Option<Arc<duckdb::InterruptHandle>>>>);

impl CancelHandle {
    pub fn cancel(&self) {
        if let Some(h) = self.0.lock().unwrap().as_ref() {
            h.interrupt();
        }
    }

    fn set(&self, h: Option<Arc<duckdb::InterruptHandle>>) {
        *self.0.lock().unwrap() = h;
    }
}

pub struct AnalyticsWorker {
    jobs: SyncSender<Job>,
    cancel: CancelHandle,
    db_path: PathBuf,
    limits: Limits,
}

impl AnalyticsWorker {
    /// Starts the worker for the SQLite database at `db_path`. No snapshot
    /// is taken until [`refresh`](Self::refresh) is called.
    pub fn spawn(db_path: PathBuf, limits: Limits) -> std::io::Result<Self> {
        let (jobs, rx) = mpsc::sync_channel::<Job>(QUEUE);
        thread::Builder::new().name("juttusihti-analytics".into()).spawn(move || {
            let mut analytics = None;
            for job in rx {
                job(&mut analytics);
            }
        })?;
        Ok(Self { jobs, cancel: CancelHandle::default(), db_path, limits })
    }

    pub fn cancel_handle(&self) -> CancelHandle {
        self.cancel.clone()
    }

    /// Replaces the snapshot with fresh data read from SQLite (read-only).
    pub fn refresh(&self) -> Result<Receiver<Result<SnapshotStats>>> {
        let (path, limits, cancel) = (self.db_path.clone(), self.limits.clone(), self.cancel.clone());
        self.submit(move |slot| {
            let sqlite = rusqlite::Connection::open_with_flags(
                &path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            sqlite.busy_timeout(std::time::Duration::from_secs(5))?;
            let fresh = Analytics::snapshot(&sqlite, &limits)?;
            let stats = fresh.stats();
            cancel.set(Some(fresh.interrupt_handle()));
            *slot = Some(fresh);
            Ok(stats)
        })
    }

    /// Runs a report against the current snapshot, taking one first if needed.
    pub fn run<T: Send + 'static>(
        &self,
        report: impl FnOnce(&Analytics) -> Result<T> + Send + 'static,
    ) -> Result<Receiver<Result<T>>> {
        let (path, limits, cancel) = (self.db_path.clone(), self.limits.clone(), self.cancel.clone());
        self.submit(move |slot| {
            if slot.is_none() {
                let sqlite = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
                let fresh = Analytics::snapshot(&sqlite, &limits)?;
                cancel.set(Some(fresh.interrupt_handle()));
                *slot = Some(fresh);
            }
            report(slot.as_ref().expect("snapshot present")).map_err(map_interrupt)
        })
    }

    fn submit<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Option<Analytics>) -> Result<T> + Send + 'static,
    ) -> Result<Receiver<Result<T>>> {
        let (tx, rx) = mpsc::channel();
        let job: Job = Box::new(move |slot| {
            let _ = tx.send(f(slot));
        });
        match self.jobs.try_send(job) {
            Ok(()) => Ok(rx),
            Err(TrySendError::Full(_)) => Err(Error::Busy),
            Err(TrySendError::Disconnected(_)) => Err(Error::WorkerGone),
        }
    }
}

fn map_interrupt(e: Error) -> Error {
    match &e {
        Error::DuckDb(d) if d.to_string().to_lowercase().contains("interrupt") => Error::Cancelled,
        _ => e,
    }
}
