//! Stand-in for juttusihti-browser used by the full_article tests.
//! The request URL picks the behaviour: "hang" sleeps, "crash" exits 3,
//! "garbage" prints junk, anything else echoes a Fetched outcome.
use std::io::{Read, Write};

use juttusihti_article::{FetchOutcome, FetchRequest};

fn main() {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();
    let req: FetchRequest = serde_json::from_str(&input).unwrap();
    std::fs::create_dir(&req.profile_dir).unwrap();
    if req.url.contains("hang") {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
    if req.url.contains("crash") {
        std::process::exit(3);
    }
    if req.url.contains("garbage") {
        println!("not json");
        return;
    }
    let out = FetchOutcome::Fetched { text: format!("text of {}", req.url), paywalled: false, picture: None };
    println!("{}", serde_json::to_string(&out).unwrap());
    std::io::stdout().flush().unwrap();
    let _ = std::fs::remove_dir_all(&req.profile_dir);
}
