//! Juttusihti's private article fetcher: a headless Servo in a throwaway profile.
//!
//! For now only a probe CLI: `juttusihti-browser --probe <url>` prints the extracted article as
//! JSON on stdout, or the error on stderr with exit code 1.

mod engine;

// Exports the GPU selection symbols surfman looks for, so it can pick the low-power adapter
// (and does not print a warning on stdout, which carries the JSON).
surfman::declare_surfman!();

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use url::Url;

const PROBE_TIMEOUT: Duration = Duration::from_secs(60);

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = match args.as_slice() {
        [flag, url] if flag == "--probe" => match Url::parse(url) {
            Ok(url) => url,
            Err(e) => {
                eprintln!("invalid URL {url}: {e}");
                return ExitCode::FAILURE;
            }
        },
        _ => {
            eprintln!("usage: juttusihti-browser --probe <url>");
            return ExitCode::FAILURE;
        }
    };

    let profile_dir = fresh_profile_dir();
    if let Err(e) = std::fs::create_dir_all(&profile_dir) {
        eprintln!("cannot create profile directory {}: {e}", profile_dir.display());
        return ExitCode::FAILURE;
    }
    let result = engine::load_and_extract(&url, &profile_dir, PROBE_TIMEOUT);
    let _ = std::fs::remove_dir_all(&profile_dir);

    match result {
        Ok(extracted) => {
            println!("{}", serde_json::to_string_pretty(&extracted).expect("Extracted serializes"));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e:?}: {e}");
            ExitCode::FAILURE
        }
    }
}

/// A new, not yet existing directory under the system temp directory.
fn fresh_profile_dir() -> PathBuf {
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    std::env::temp_dir().join(format!("juttusihti-browser-{}-{nanos}", std::process::id()))
}
