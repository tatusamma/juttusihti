//! Juttusihti's private article fetcher: a headless Servo in a throwaway profile.
//!
//! The app starts one helper per fetch. It reads one `FetchRequest` JSON from stdin and writes
//! exactly one `FetchOutcome` JSON line to stdout; diagnostics go to stderr. The exit code is 0
//! for every outcome. The profile directory is created by the helper (it must not exist yet) and
//! removed before the helper exits, whatever happened in between.

mod blocker;
mod consent;
mod engine;
mod outcome_stdout;
mod profile;

// Exports the GPU selection symbols surfman looks for, so it can pick the low-power adapter
// (and does not print a warning on stdout, which carries the JSON).
surfman::declare_surfman!();

use std::io::{Read, Write};
use std::panic::{self, AssertUnwindSafe};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use juttusihti_article::{FetchOutcome, FetchRequest, MAX_PICTURE_INPUT_BYTES, process_picture};
use url::Url;

use crate::engine::{EngineError, PageLoad};
use crate::profile::Profile;

fn main() {
    // Before anything can print: the real stdout is kept for the outcome line only, and all other
    // stdout output (Servo's, including page-chosen text) goes to stderr.
    let mut out = match outcome_stdout::take() {
        Ok(out) => out,
        Err(e) => {
            // Without a protected stdout the outcome could be forged by the page; do not load it.
            let outcome = FetchOutcome::Failed { message: format!("cannot set up the output: {e}") };
            println!("{}", serde_json::to_string(&outcome).expect("FetchOutcome serializes"));
            return;
        }
    };
    let outcome = match read_request() {
        Ok(request) => fetch(&request),
        Err(message) => FetchOutcome::Failed { message },
    };
    let line = serde_json::to_string(&outcome).expect("FetchOutcome serializes");
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn read_request() -> Result<FetchRequest, String> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).map_err(|e| format!("cannot read the request: {e}"))?;
    serde_json::from_str(&input).map_err(|e| format!("invalid request: {e}"))
}

fn fetch(request: &FetchRequest) -> FetchOutcome {
    let url = match Url::parse(&request.url) {
        Ok(url) if matches!(url.scheme(), "http" | "https") => url,
        Ok(url) => return FetchOutcome::Failed { message: format!("not a web address: {url}") },
        Err(e) => return FetchOutcome::Failed { message: format!("invalid URL {}: {e}", request.url) },
    };
    let profile = match Profile::create(&request.profile_dir) {
        Ok(profile) => profile,
        Err(e) => {
            return FetchOutcome::Failed {
                message: format!("cannot create the profile directory {}: {e}", request.profile_dir.display()),
            };
        }
    };

    // A panic must still produce one outcome line; the profile guard removes the directory either way.
    let outcome = panic::catch_unwind(AssertUnwindSafe(|| fetch_in_profile(request, &url, profile.path())))
        .unwrap_or_else(|_| FetchOutcome::Failed { message: "the browser engine panicked".to_string() });
    profile.remove();
    outcome
}

fn fetch_in_profile(request: &FetchRequest, url: &Url, profile_dir: &Path) -> FetchOutcome {
    let page = PageLoad {
        url,
        profile_dir,
        timeout: Duration::from_secs(u64::from(request.timeout_secs)),
        want_picture: request.want_picture,
        max_picture_bytes: MAX_PICTURE_INPUT_BYTES,
    };
    let (result, stats) = engine::load_and_extract(&page, Rc::new(blocker::Blocker::bundled()));
    eprintln!("juttusihti-browser: {} requests, {} blocked", stats.requests, stats.blocked);

    match result {
        Ok(loaded) => {
            if let Some(url) = &loaded.extracted.picture_url {
                eprintln!("juttusihti-browser: picture {url}");
            }
            let picture = loaded.picture.and_then(|bytes| process_picture(&bytes, &request.picture_path));
            FetchOutcome::Fetched { text: loaded.extracted.text, paywalled: loaded.extracted.paywalled, picture }
        }
        Err(EngineError::Refused(status)) => FetchOutcome::Refused { status },
        Err(EngineError::Timeout) => FetchOutcome::Timeout,
        Err(EngineError::NoArticle) => FetchOutcome::NoArticle,
        Err(EngineError::Engine(message)) => FetchOutcome::Failed { message },
    }
}
