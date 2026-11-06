//! Drives the real helper binary (one `FetchRequest` on stdin, one `FetchOutcome` line on stdout)
//! against pages served by a local HTTP server.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use juttusihti_article::{FetchOutcome, FetchRequest, PictureInfo};

/// A helper run that takes longer than this is killed and the test fails.
const HELPER_TEST_TIMEOUT: Duration = Duration::from_secs(120);

struct Server {
    port: u16,
    /// Request paths in arrival order.
    log: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn start() -> Server {
        Server::start_with_picture_origin(None)
    }

    /// `picture_origin` replaces `PICTURE_ORIGIN` in the cross-origin fixture.
    fn start_with_picture_origin(picture_origin: Option<String>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let log = Arc::new(Mutex::new(Vec::new()));
        let files = Arc::new(Files {
            hero: jpeg(1000, 600),
            lead: jpeg(1200, 800),
            site_default: jpeg(300, 300),
            picture_origin: picture_origin.unwrap_or_default(),
        });
        let server_log = log.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = server_log.clone();
                let files = files.clone();
                thread::spawn(move || answer(stream, &log, &files));
            }
        });
        Server { port, log }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    fn requested(&self, path: &str) -> bool {
        self.log.lock().unwrap().iter().any(|p| p == path)
    }
}

struct Files {
    hero: Vec<u8>,
    lead: Vec<u8>,
    site_default: Vec<u8>,
    picture_origin: String,
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    let img = image::RgbImage::from_fn(width, height, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
    let mut out = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(img).write_to(&mut out, image::ImageFormat::Jpeg).expect("encode jpeg");
    out.into_inner()
}

fn answer(mut stream: TcpStream, log: &Mutex<Vec<String>>, files: &Files) {
    let mut request_line = String::new();
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Drain the headers.
    let mut line = String::new();
    while reader.read_line(&mut line).is_ok_and(|n| n > 2) {
        line.clear();
    }
    let path = request_line.split_whitespace().nth(1).unwrap_or("").to_string();
    log.lock().unwrap().push(path.clone());
    if path == "/hang" {
        // Never answers within the helper's timeout.
        thread::sleep(Duration::from_secs(30));
        return;
    }

    let html = |body: &str| ("200 OK", "text/html; charset=utf-8", body.as_bytes().to_vec());
    let cross = include_str!("fixtures/cross.html").replace("PICTURE_ORIGIN", &files.picture_origin);
    let (status, content_type, body) = match path.as_str() {
        "/article" => html(include_str!("fixtures/article.html")),
        "/lead" => html(include_str!("fixtures/lead.html")),
        "/state" => html(include_str!("fixtures/state.html")),
        "/sw" => html(include_str!("fixtures/sw.html")),
        "/consent" => html(include_str!("fixtures/consent.html")),
        "/cross" => html(&cross),
        "/forge" => html(include_str!("fixtures/forge.html")),
        "/sw.js" => ("200 OK", "text/javascript", b"self.addEventListener('fetch', function () {});".to_vec()),
        "/hero.jpg" => ("200 OK", "image/jpeg", files.hero.clone()),
        "/lead.jpg" => ("200 OK", "image/jpeg", files.lead.clone()),
        "/some/site-default.jpg" => ("200 OK", "image/jpeg", files.site_default.clone()),
        _ => ("404 Not Found", "text/plain", Vec::new()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&body);
}

/// Runs the helper once with `request` on stdin and returns its single outcome line.
fn run_helper(request: &FetchRequest) -> FetchOutcome {
    run_helper_with_stderr(request).0
}

/// Like `run_helper`, also returning the helper's stderr.
fn run_helper_with_stderr(request: &FetchRequest) -> (FetchOutcome, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_juttusihti-browser"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start helper");
    let input = serde_json::to_vec(request).expect("request serializes");
    child.stdin.take().expect("stdin").write_all(&input).expect("write request");

    let mut stdout = child.stdout.take().expect("stdout");
    let mut stderr = child.stderr.take().expect("stderr");
    let out_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_reader = thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for helper") {
            break status;
        }
        if started.elapsed() > HELPER_TEST_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            panic!("helper did not finish within {HELPER_TEST_TIMEOUT:?}");
        }
        thread::sleep(Duration::from_millis(50));
    };
    let stdout = out_reader.join().expect("stdout reader");
    let stderr = String::from_utf8_lossy(&err_reader.join().expect("stderr reader")).into_owned();
    assert!(status.success(), "helper exit status {status}; stderr:\n{stderr}");

    let stdout = String::from_utf8(stdout).expect("stdout is UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "stdout must be exactly one line, got {stdout:?}; stderr:\n{stderr}");
    let outcome =
        serde_json::from_str(lines[0]).unwrap_or_else(|e| panic!("bad outcome {:?}: {e}; stderr:\n{stderr}", lines[0]));
    (outcome, stderr)
}

fn request(url: String, dir: &Path, name: &str, want_picture: bool) -> FetchRequest {
    FetchRequest {
        url,
        want_picture,
        profile_dir: dir.join(format!("{name}-profile")),
        picture_path: dir.join(format!("{name}.jpg")),
        timeout_secs: 45,
    }
}

fn fetched(outcome: FetchOutcome) -> (String, bool, Option<PictureInfo>) {
    match outcome {
        FetchOutcome::Fetched { text, paywalled, picture } => (text, paywalled, picture),
        other => panic!("expected Fetched, got {other:?}"),
    }
}

#[test]
fn extracts_text_from_local_article() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let req = request(server.url("/article"), dir.path(), "a", false);
    let (text, paywalled, picture) = fetched(run_helper(&req));

    assert!(text.contains("Councillors argued for three hours about the cost of the bridge"), "text: {text}");
    // Paragraphs survive as blank-line separated blocks.
    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    assert!(paragraphs.len() >= 3, "paragraphs: {paragraphs:?}");
    assert!(paragraphs.iter().any(|p| p.starts_with("The second paragraph")), "paragraphs: {paragraphs:?}");
    assert!(!text.contains("teaser card"), "teaser card of another story in the text: {text}");
    assert!(paywalled);
    assert!(picture.is_none());
    assert!(!req.profile_dir.exists(), "profile dir left behind");
}

#[test]
fn second_fetch_sees_no_state_from_first() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let first = request(server.url("/state"), dir.path(), "first", false);
    let second = request(server.url("/state"), dir.path(), "second", false);

    let (text1, _, _) = fetched(run_helper(&first));
    assert!(text1.contains("cookie= and storage= from"), "first run text: {text1}");
    let (text2, _, _) = fetched(run_helper(&second));
    assert!(text2.contains("cookie= and storage= from"), "second run saw state: {text2}");

    assert!(!first.profile_dir.exists(), "first profile dir left behind");
    assert!(!second.profile_dir.exists(), "second profile dir left behind");
}

#[test]
fn picture_written_when_wanted() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();

    let with = request(server.url("/article"), dir.path(), "with", true);
    let (_, _, picture) = fetched(run_helper(&with));
    let picture = picture.expect("picture");
    assert_eq!(picture.path, with.picture_path);
    assert!(with.picture_path.exists());
    assert_eq!((picture.width, picture.height), (1000, 600));
    assert!(server.requested("/hero.jpg"));

    let server = Server::start();
    let without = request(server.url("/article"), dir.path(), "without", false);
    let (_, _, picture) = fetched(run_helper(&without));
    assert!(picture.is_none());
    assert!(!without.picture_path.exists());
    assert!(!server.requested("/hero.jpg"), "picture was requested: {:?}", server.log.lock().unwrap());

    assert!(!with.profile_dir.exists() && !without.profile_dir.exists(), "profile dir left behind");
}

#[test]
fn picture_prefers_article_image_over_site_default() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let req = request(server.url("/lead"), dir.path(), "lead", true);
    let (_, _, picture) = fetched(run_helper(&req));
    let picture = picture.expect("picture");
    assert_eq!((picture.width, picture.height), (1200, 800), "picked the site default instead of the lead image");
    assert!(!server.requested("/some/site-default.jpg"));
    assert!(!req.profile_dir.exists(), "profile dir left behind");
}

#[test]
fn service_worker_registration_is_refused() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let first = request(server.url("/sw"), dir.path(), "sw1", false);
    let second = request(server.url("/sw"), dir.path(), "sw2", false);

    let _ = fetched(run_helper(&first));
    let (text, _, _) = fetched(run_helper(&second));
    assert!(
        text.contains("sw=unsupported") || text.contains("sw=registrations-0 "),
        "second run found a registration: {text}"
    );
    assert!(!server.requested("/sw.js"), "service worker script was fetched");
    assert!(!first.profile_dir.exists() && !second.profile_dir.exists(), "profile dir left behind");
}

#[test]
fn existing_profile_dir_is_refused_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let req = request("http://127.0.0.1:9/never".to_string(), dir.path(), "taken", false);
    std::fs::create_dir(&req.profile_dir).unwrap();
    std::fs::write(req.profile_dir.join("keep.txt"), b"not the helper's").unwrap();

    match run_helper(&req) {
        FetchOutcome::Failed { .. } => {}
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(req.profile_dir.join("keep.txt").exists(), "helper deleted a directory it did not create");
}

#[test]
fn consent_banner_is_accepted() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let req = request(server.url("/consent"), dir.path(), "consent", false);
    let (text, _, _) = fetched(run_helper(&req));
    // Accepted as the only answer: no reject click came first.
    assert!(text.contains("Consent answers in order: accepted, by the helper."), "text: {text}");
    assert!(!req.profile_dir.exists(), "profile dir left behind");
}

#[test]
fn cross_origin_picture_is_downloaded_in_the_same_session() {
    let pictures = Server::start();
    let server = Server::start_with_picture_origin(Some(pictures.url("")));
    let dir = tempfile::tempdir().unwrap();
    let req = request(server.url("/cross"), dir.path(), "cross", true);
    let (text, _, picture) = fetched(run_helper(&req));
    assert!(text.contains("harbour authority"), "text: {text}");
    let picture = picture.expect("picture");
    assert_eq!((picture.width, picture.height), (1200, 800));
    assert!(pictures.requested("/lead.jpg"));
    assert!(!req.profile_dir.exists(), "profile dir left behind");
}

#[test]
fn hanging_page_times_out_and_profile_is_removed() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let mut req = request(server.url("/hang"), dir.path(), "hang", false);
    req.timeout_secs = 3;
    let started = Instant::now();
    assert_eq!(run_helper(&req), FetchOutcome::Timeout);
    assert!(started.elapsed() < Duration::from_secs(20), "took {:?}", started.elapsed());
    assert!(!req.profile_dir.exists(), "profile dir left behind");
}

#[test]
fn page_output_cannot_reach_stdout() {
    let server = Server::start();
    let dir = tempfile::tempdir().unwrap();
    let req = request(server.url("/forge"), dir.path(), "forge", false);
    // run_helper asserts that stdout is exactly one line.
    let (text, _, picture) = fetched(run_helper(&req));
    assert!(text.contains("single real outcome line"), "text: {text}");
    assert!(picture.is_none());
    assert!(!req.profile_dir.exists(), "profile dir left behind");
}

/// The cross-origin fixture with its pictures on a non-web origin: no picture is read.
fn no_picture_from_origin(origin: String, name: &str) {
    let server = Server::start_with_picture_origin(Some(origin));
    let dir = tempfile::tempdir().unwrap();
    let req = request(server.url("/cross"), dir.path(), name, true);
    let (outcome, stderr) = run_helper_with_stderr(&req);
    let (text, _, picture) = fetched(outcome);
    assert!(text.contains("harbour authority"), "text: {text}");
    assert!(picture.is_none(), "picture read from a non-web origin: {picture:?}");
    assert!(!stderr.contains("juttusihti-browser: picture "), "a non-web picture was chosen:\n{stderr}");
    assert!(!req.picture_path.exists());
    assert!(!req.profile_dir.exists(), "profile dir left behind");
}

#[test]
fn local_file_picture_is_never_read() {
    let files = tempfile::tempdir().unwrap();
    std::fs::write(files.path().join("lead.jpg"), jpeg(1200, 800)).unwrap();
    let origin = url::Url::from_directory_path(files.path()).unwrap().to_string();
    no_picture_from_origin(origin.trim_end_matches('/').to_string(), "file");
}

#[test]
fn non_web_scheme_picture_is_never_read() {
    let pictures = Server::start();
    no_picture_from_origin(format!("ftp://127.0.0.1:{}", pictures.port), "ftp");
    assert!(pictures.log.lock().unwrap().is_empty(), "something reached the picture server");
}
