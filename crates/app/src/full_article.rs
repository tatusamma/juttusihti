//! Runs the `juttusihti-browser` helper for one "Read full article" click.
//!
//! The helper is a separate process so a browser crash or hang cannot take
//! the app down. Each fetch gets a fresh profile directory (which the helper
//! creates itself and deletes before exit) under `browser-profiles/`.

#![allow(dead_code)]

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use juttusihti_article::{FetchOutcome, FetchRequest};
use juttusihti_core::model::ArticleId;
use juttusihti_core::store::Store;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use url::Url;

/// Hard limit for one fetch; the helper itself is told to stop earlier.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const HELPER_TIMEOUT_SECS: u32 = 45;
/// Upper bound for the one outcome line read from the helper.
const MAX_LINE_BYTES: u64 = 32 * 1024 * 1024;

const PROFILES_DIR: &str = "browser-profiles";
const FULLTEXT_DIR: &str = "fulltext";

/// Where the helper binary lives: `JUTTUSIHTI_BROWSER`, else next to the app.
pub fn helper_path() -> PathBuf {
    if let Some(p) = std::env::var_os("JUTTUSIHTI_BROWSER") {
        return PathBuf::from(p);
    }
    let name = if cfg!(windows) { "juttusihti-browser.exe" } else { "juttusihti-browser" };
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join(name))).unwrap_or_else(|| PathBuf::from(name))
}

/// Accepts only http and https article links.
pub fn check_url(url: Option<&str>) -> Result<Url, &'static str> {
    let raw = url.ok_or("This article has no link.")?;
    let parsed = Url::parse(raw.trim()).map_err(|_| "This article has an invalid link.")?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed),
        _ => Err("Only http and https links can be opened."),
    }
}

fn failed(message: &str) -> FetchOutcome {
    FetchOutcome::Failed { message: message.to_string() }
}

fn unique_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!("{:x}{:x}{:x}", nanos, std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub async fn fetch(
    helper: PathBuf,
    data_dir: PathBuf,
    article_id: ArticleId,
    url: Url,
    want_picture: bool,
) -> FetchOutcome {
    fetch_with_timeout(helper, data_dir, article_id, url, want_picture, FETCH_TIMEOUT).await
}

async fn fetch_with_timeout(
    helper: PathBuf,
    data_dir: PathBuf,
    article_id: ArticleId,
    url: Url,
    want_picture: bool,
    timeout: Duration,
) -> FetchOutcome {
    let profiles = data_dir.join(PROFILES_DIR);
    let fulltext = data_dir.join(FULLTEXT_DIR);
    // The helper creates the profile dir itself, so only its parent exists.
    let profile_dir = profiles.join(format!("{article_id}-{}", unique_suffix()));
    if std::fs::create_dir_all(&profiles).is_err() || std::fs::create_dir_all(&fulltext).is_err() {
        return failed("Could not prepare the cache folders.");
    }
    let request = FetchRequest {
        url: url.to_string(),
        want_picture,
        profile_dir: profile_dir.clone(),
        picture_path: fulltext.join(format!("{article_id}.jpg")),
        timeout_secs: HELPER_TIMEOUT_SECS,
    };
    let outcome = check_picture(run_helper(&helper, &request, timeout).await, &request.picture_path);
    // Backstop: the helper removes it, but a kill or crash may leave it behind.
    let _ = std::fs::remove_dir_all(&profile_dir);
    outcome
}

/// The picture must be exactly the file the app asked for; anything else is
/// dropped (text kept) so a foreign path never reaches the database.
fn check_picture(outcome: FetchOutcome, expected: &Path) -> FetchOutcome {
    match outcome {
        FetchOutcome::Fetched { text, paywalled, picture } => {
            let ok = picture.as_ref().is_some_and(|p| p.path == expected && expected.is_file());
            if !ok {
                let _ = std::fs::remove_file(expected);
            }
            FetchOutcome::Fetched { text, paywalled, picture: if ok { picture } else { None } }
        }
        other => other,
    }
}

async fn run_helper(helper: &Path, request: &FetchRequest, timeout: Duration) -> FetchOutcome {
    let Ok(body) = serde_json::to_vec(request) else { return failed("Could not build the request.") };
    let mut cmd = tokio::process::Command::new(helper);
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return failed("The browser helper is missing."),
        Err(_) => return failed("The browser helper could not be started."),
    };
    let (Some(mut stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        let _ = child.kill().await;
        return failed("The browser helper could not be started.");
    };
    let io = async {
        // A helper that failed early may not read stdin; a broken pipe here
        // is not fatal, its stdout and exit status still tell the story.
        let _ = stdin.write_all(&body).await;
        let _ = stdin.shutdown().await;
        drop(stdin);
        let mut line = String::new();
        let mut reader = BufReader::new(stdout.take(MAX_LINE_BYTES));
        let read = reader.read_line(&mut line).await;
        let status = child.wait().await;
        (read, line, status)
    };
    match tokio::time::timeout(timeout, io).await {
        Err(_) => {
            let _ = child.kill().await;
            FetchOutcome::Timeout
        }
        Ok((read, line, status)) => {
            if line.trim().is_empty() || read.is_err() {
                return failed("The browser helper gave no result.");
            }
            match serde_json::from_str::<FetchOutcome>(line.trim()) {
                Ok(outcome) => match status {
                    Ok(s) if s.success() => outcome,
                    _ => failed("The browser helper exited abnormally."),
                },
                Err(_) => failed("The browser helper gave an unreadable result."),
            }
        }
    }
}

/// Deletes cache files by path relative to `dir`; a missing file is fine.
/// Absolute paths and `..` components are refused.
pub fn delete_cache_files(dir: &Path, paths: &[String]) -> Result<(), String> {
    for p in paths {
        let rel = Path::new(p);
        if p.is_empty() || !rel.components().all(|c| matches!(c, Component::Normal(_))) {
            return Err(format!("Refusing to delete unsafe path {p}"));
        }
        match std::fs::remove_file(dir.join(rel)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Could not delete {p}: {e}")),
        }
    }
    Ok(())
}

/// Runs before the UI starts: clears profile leftovers, trims the cache to
/// its limit and removes picture files the database no longer knows. Errors
/// are logged, never fatal.
pub fn startup_sweep(data_dir: &Path, store: &Store, limit_bytes: u64) {
    let profiles = data_dir.join(PROFILES_DIR);
    if profiles.exists()
        && let Err(e) = std::fs::remove_dir_all(&profiles)
    {
        tracing::warn!(error = %e, "could not clear browser profiles");
    }
    match store.enforce_fulltext_limit(limit_bytes) {
        Ok(removed) => {
            if let Err(e) = delete_cache_files(data_dir, &removed) {
                tracing::warn!(error = %e, "could not delete evicted pictures");
            }
        }
        Err(e) => tracing::warn!(error = %e, "could not enforce the full article cache limit"),
    }
    let known = match store.fulltext_picture_paths() {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, "could not list cached pictures");
            return;
        }
    };
    let Ok(entries) = std::fs::read_dir(data_dir.join(FULLTEXT_DIR)) else { return };
    let orphans: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .filter_map(|e| e.file_name().to_str().map(|n| format!("{FULLTEXT_DIR}/{n}")))
        .filter(|rel| !known.contains(rel))
        .collect();
    if let Err(e) = delete_cache_files(data_dir, &orphans) {
        tracing::warn!(error = %e, "could not delete orphaned pictures");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_helper() -> PathBuf {
        // Test exes live in target/<profile>/deps, examples in ../examples.
        let exe = std::env::current_exe().unwrap();
        let name = if cfg!(windows) { "fake_helper.exe" } else { "fake_helper" };
        let p = exe.parent().unwrap().parent().unwrap().join("examples").join(name);
        assert!(p.exists(), "build the example first: {}", p.display());
        p
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn non_http_url_is_rejected_before_spawn() {
        assert!(check_url(None).is_err());
        assert!(check_url(Some("javascript:alert(1)")).is_err());
        assert!(check_url(Some("file:///c:/x")).is_err());
        assert!(check_url(Some("ftp://x")).is_err());
        assert!(check_url(Some("https://www.hs.fi/a.html")).is_ok());
    }

    #[tokio::test]
    async fn fake_helper_outcome_is_returned() {
        let dir = tempfile::tempdir().unwrap();
        let out = fetch(fake_helper(), dir.path().into(), 7, url("https://x.fi/a"), false).await;
        match out {
            FetchOutcome::Fetched { text, .. } => assert_eq!(text, "text of https://x.fi/a"),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn hung_helper_is_killed_and_profile_removed() {
        let dir = tempfile::tempdir().unwrap();
        let start = std::time::Instant::now();
        let out = fetch_with_timeout(
            fake_helper(),
            dir.path().into(),
            7,
            url("https://x.fi/hang"),
            false,
            Duration::from_secs(2),
        )
        .await;
        assert!(matches!(out, FetchOutcome::Timeout), "{out:?}");
        assert!(start.elapsed() < Duration::from_secs(4));
        let left = std::fs::read_dir(dir.path().join(PROFILES_DIR)).unwrap().count();
        assert_eq!(left, 0);
    }

    #[tokio::test]
    async fn missing_helper_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let out = fetch(dir.path().join("nope.exe"), dir.path().into(), 1, url("https://x.fi/"), false).await;
        assert!(matches!(out, FetchOutcome::Failed { .. }), "{out:?}");
    }

    #[tokio::test]
    async fn crash_and_garbage_are_failed() {
        let dir = tempfile::tempdir().unwrap();
        for u in ["https://x.fi/crash", "https://x.fi/garbage"] {
            let out = fetch(fake_helper(), dir.path().into(), 1, url(u), false).await;
            assert!(matches!(out, FetchOutcome::Failed { .. }), "{u}: {out:?}");
        }
    }

    #[test]
    fn delete_guard_rejects_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("keep.txt");
        std::fs::write(&outside, "x").unwrap();
        let sub = dir.path().join("data");
        std::fs::create_dir(&sub).unwrap();
        assert!(delete_cache_files(&sub, &["../keep.txt".into()]).is_err());
        assert!(delete_cache_files(&sub, &[outside.to_string_lossy().into_owned()]).is_err());
        assert!(delete_cache_files(&sub, &["".into()]).is_err());
        assert!(outside.exists());
        assert!(delete_cache_files(&sub, &["fulltext/missing.jpg".into()]).is_ok());
    }

    #[tokio::test]
    async fn foreign_or_missing_picture_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        for u in ["https://x.fi/foreignpic", "https://x.fi/missingpic"] {
            let out = fetch(fake_helper(), dir.path().into(), 5, url(u), true).await;
            match out {
                FetchOutcome::Fetched { picture, .. } => assert!(picture.is_none(), "{u}"),
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(!dir.path().join(FULLTEXT_DIR).join("5.jpg").exists());
    }

    #[tokio::test]
    async fn expected_picture_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let out = fetch(fake_helper(), dir.path().into(), 5, url("https://x.fi/goodpic"), true).await;
        match out {
            FetchOutcome::Fetched { picture, .. } => {
                assert_eq!(picture.unwrap().path, dir.path().join(FULLTEXT_DIR).join("5.jpg"))
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn startup_sweep_removes_leftovers_and_orphans() {
        use juttusihti_core::{NewArticle, NewFeed};
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(&dir.path().join("t.db")).unwrap();
        let feed = store
            .add_feed(&NewFeed { url: "https://f.example/rss".into(), title: "F".into(), ..Default::default() })
            .unwrap();
        let entries: Vec<NewArticle> = (1..=2)
            .map(|i| NewArticle {
                guid: i.to_string(),
                url: Some(format!("https://n.example/{i}")),
                title: format!("T{i}"),
                summary: None,
                content: None,
                author: None,
                published_at: Some(1_790_812_800 + i),
            })
            .collect();
        store.ingest(feed, &entries).unwrap();
        let ids: Vec<i64> = {
            let mut st = store.conn().prepare("SELECT id FROM articles ORDER BY id").unwrap();
            st.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap()
        };
        let ft = dir.path().join(FULLTEXT_DIR);
        std::fs::create_dir_all(&ft).unwrap();
        std::fs::create_dir_all(dir.path().join(PROFILES_DIR).join("1-abc")).unwrap();
        std::fs::write(dir.path().join(PROFILES_DIR).join("1-abc").join("f"), "x").unwrap();
        std::fs::write(ft.join("99.jpg"), "x").unwrap();
        let (a, b) = (format!("fulltext/{}.jpg", ids[0]), format!("fulltext/{}.jpg", ids[1]));
        std::fs::write(dir.path().join(&a), "x").unwrap();
        std::fs::write(dir.path().join(&b), "x").unwrap();
        store.save_fulltext(ids[0], "old", false, Some((&a, 1000))).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        store.save_fulltext(ids[1], "new", false, Some((&b, 1000))).unwrap();

        // Under the limit: both rows' files survive, orphan and profile go.
        startup_sweep(dir.path(), &store, 1_000_000);
        assert!(!dir.path().join(PROFILES_DIR).exists());
        assert!(!ft.join("99.jpg").exists());
        assert!(dir.path().join(&a).exists());
        assert!(dir.path().join(&b).exists());

        // Over the limit: the oldest is evicted with its file, the newer stays.
        startup_sweep(dir.path(), &store, 1_500);
        assert!(!dir.path().join(&a).exists());
        assert!(dir.path().join(&b).exists());
    }
}
