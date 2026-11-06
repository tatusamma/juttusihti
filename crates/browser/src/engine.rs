//! Loads one page in a headless Servo with tracker blocking and consent handling, and extracts
//! the article (and, when wanted, the bytes of its main picture) from it.
//!
//! The user agent is exactly the one Servo ships with and is never changed (see the design's
//! browser identity rule). Nothing is retried: a refused or broken page is reported as it is.

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use content_security_policy::Destination;
use serde::{Deserialize, Serialize};
use servo::{
    JSValue, LoadStatus, Opts, PermissionRequest, Preferences, RenderingContext, Servo, ServoBuilder, ServoDelegate,
    SoftwareRenderingContext, UserContentManager, UserScript, WebResourceLoad, WebResourceRequest, WebResourceResponse,
    WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;

use crate::blocker::{Blocker, ResourceKind};
use crate::consent;

const READABILITY_JS: &str = include_str!("../assets/Readability.js");
const EXTRACT_JS: &str = include_str!("../assets/extract.js");

/// Shorter article text than this means the page had no article.
const MIN_ARTICLE_CHARS: usize = 200;

/// How long autoconsent may look for and answer a consent banner after the page has loaded.
const CONSENT_BUDGET: Duration = Duration::from_secs(6);

/// How long to wait for a navigation the consent answer triggered (some banners reload the page).
const CONSENT_NAVIGATION_WAIT: Duration = Duration::from_secs(5);

/// How long the picture download may take, within the overall deadline.
const PICTURE_BUDGET: Duration = Duration::from_secs(15);

/// How often page state is polled while waiting.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The viewport the page is laid out in. Nothing is shown; it only shapes layout.
const VIEWPORT: dpi::PhysicalSize<u32> = dpi::PhysicalSize { width: 1280, height: 900 };

/// Paragraphs that are site controls or labels, not article text (compared case-insensitively).
const UI_NOISE: &[&str] = &[
    "lue tiivistelmä",
    "luitko jo nämä?",
    "mainos",
    "mainos päättyy",
    "jaa",
    "jaa artikkeli",
    "kommentoi",
    "kommentit",
    "kuuntele",
    "kuuntele artikkeli",
    "lue lisää",
    "lue lisää kirjoittajalta",
    "lue myös",
    "seuraa ja lue artikkeliin liittyviä aiheita",
    "tilaajille",
    "x-sisältöä ei voida näyttää",
    "advertisement",
    "share",
    "share this article",
];

/// What was extracted from a loaded article page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Extracted {
    /// HTTP status of the main document, when the engine exposes it. Servo 0.7 does not: its
    /// resource-load hooks see requests only, never responses, and it has no navigation-timing
    /// `responseStatus`. So a refusal page (403) surfaces as `NoArticle`, never as `Refused`.
    pub main_status: Option<u16>,
    pub title: String,
    /// Article text, paragraphs separated by blank lines.
    pub text: String,
    pub picture_url: Option<String>,
    /// The page's JSON-LD says the article is not free (`isAccessibleForFree: false`).
    pub paywalled: bool,
}

/// The article and, when it was wanted and could be downloaded, the main picture's bytes.
#[derive(Debug)]
pub struct Loaded {
    pub extracted: Extracted,
    pub picture: Option<Vec<u8>>,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("the site refused the page with HTTP {0}")]
    Refused(u16),
    #[error("the page did not finish loading in time")]
    Timeout,
    #[error("no article found on the page")]
    NoArticle,
    #[error("browser engine error: {0}")]
    Engine(String),
}

/// One page load: what to load and what to bring back.
pub struct PageLoad<'a> {
    pub url: &'a Url,
    pub profile_dir: &'a Path,
    pub timeout: Duration,
    pub want_picture: bool,
    /// Largest picture download accepted, in bytes.
    pub max_picture_bytes: usize,
}

/// Counts of resource loads, reported on stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct LoadStats {
    pub requests: u32,
    pub blocked: u32,
}

struct Delegate {
    blocker: Rc<Blocker>,
    /// Pictures are not wanted: no image is loaded at all.
    block_images: bool,
    /// URL of the current top-level document, the "source" for third-party matching.
    page_url: RefCell<Option<Url>>,
    load_complete: Cell<bool>,
    /// Top-level loads started so far.
    navigations: Cell<u32>,
    crashed: RefCell<Option<String>>,
    stats: Rc<Cell<LoadStats>>,
}

impl Delegate {
    fn should_block(&self, request: &WebResourceRequest) -> bool {
        let kind = resource_kind(request.destination);
        if request.is_for_main_frame && kind == ResourceKind::Document {
            // The page the reader asked for.
            *self.page_url.borrow_mut() = Some(request.url.clone());
            return false;
        }
        if kind == ResourceKind::Media || (self.block_images && kind == ResourceKind::Image) {
            return true;
        }
        let page = self.page_url.borrow();
        let source = request.referrer_url.as_ref().or(page.as_ref()).map(Url::as_str).unwrap_or("");
        self.blocker.should_block(request.url.as_str(), source, kind)
    }
}

impl WebViewDelegate for Delegate {
    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        match status {
            LoadStatus::Started => {
                self.navigations.set(self.navigations.get() + 1);
                self.load_complete.set(false);
            }
            LoadStatus::HeadParsed => {}
            LoadStatus::Complete => self.load_complete.set(true),
        }
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, _backtrace: Option<String>) {
        *self.crashed.borrow_mut() = Some(reason);
    }

    fn load_web_resource(&self, _webview: WebView, load: WebResourceLoad) {
        let blocked = self.should_block(load.request());
        let mut stats = self.stats.get();
        stats.requests += 1;
        stats.blocked += u32::from(blocked);
        self.stats.set(stats);
        if blocked {
            block(load);
        }
    }

    fn request_permission(&self, _webview: WebView, request: PermissionRequest) {
        request.deny();
    }
}

/// Handles loads that belong to no `WebView`, such as service worker scripts: all are refused,
/// so nothing can be registered to outlive the page.
struct BlockEverythingElse {
    stats: Rc<Cell<LoadStats>>,
}

impl ServoDelegate for BlockEverythingElse {
    fn load_web_resource(&self, load: WebResourceLoad) {
        let mut stats = self.stats.get();
        stats.requests += 1;
        stats.blocked += 1;
        self.stats.set(stats);
        block(load);
    }
}

/// Answers a load with an empty 204 instead of letting it reach the network.
fn block(load: WebResourceLoad) {
    let response = WebResourceResponse::new(load.request().url.clone())
        .status_code(http::StatusCode::NO_CONTENT)
        .status_message(b"No Content".to_vec());
    load.intercept(response).finish();
}

fn resource_kind(destination: Destination) -> ResourceKind {
    match destination {
        Destination::Document => ResourceKind::Document,
        Destination::Frame | Destination::IFrame => ResourceKind::Subdocument,
        Destination::Script
        | Destination::Worker
        | Destination::SharedWorker
        | Destination::ServiceWorker
        | Destination::AudioWorklet
        | Destination::PaintWorklet
        | Destination::Xslt => ResourceKind::Script,
        Destination::Style => ResourceKind::Stylesheet,
        Destination::Image => ResourceKind::Image,
        Destination::Font => ResourceKind::Font,
        Destination::Audio | Destination::Video | Destination::Track => ResourceKind::Media,
        Destination::Object | Destination::Embed => ResourceKind::Object,
        Destination::None | Destination::Json => ResourceKind::Xhr,
        _ => ResourceKind::Other,
    }
}

/// Loads the page in a fresh Servo whose config directory is `page.profile_dir`, answers any
/// consent banner, and extracts the article. Servo is shut down completely (all its threads
/// joined and storage closed) before this returns, so the profile directory can be removed.
pub fn load_and_extract(page: &PageLoad, blocker: Rc<Blocker>) -> (Result<Loaded, EngineError>, LoadStats) {
    let stats = Rc::new(Cell::new(LoadStats::default()));
    let result = run(page, blocker, stats.clone());
    (result, stats.get())
}

fn run(page: &PageLoad, blocker: Rc<Blocker>, stats: Rc<Cell<LoadStats>>) -> Result<Loaded, EngineError> {
    let deadline = Instant::now() + page.timeout;

    let rendering_context = Rc::new(with_current_gl_context(|| {
        SoftwareRenderingContext::new(VIEWPORT).map_err(|e| EngineError::Engine(format!("rendering context: {e:?}")))
    })??);
    rendering_context.make_current().map_err(|e| EngineError::Engine(format!("rendering context: {e:?}")))?;

    let opts = Opts { config_dir: Some(page.profile_dir.to_path_buf()), temporary_storage: true, ..Opts::default() };
    // IS and HS (Next.js) crash without IntersectionObserver, which Servo 0.7 ships behind this
    // experimental-feature pref. It only enables a standard web API; nothing about the browser's
    // identity changes. Service workers stay off (Servo's default).
    let preferences = Preferences { dom_intersection_observer_enabled: true, ..Preferences::default() };
    let servo = ServoBuilder::default().opts(opts).preferences(preferences).build();
    servo.set_delegate(Rc::new(BlockEverythingElse { stats: stats.clone() }));

    let delegate = Rc::new(Delegate {
        blocker,
        block_images: !page.want_picture,
        page_url: RefCell::new(None),
        load_complete: Cell::new(false),
        navigations: Cell::new(0),
        crashed: RefCell::new(None),
        stats,
    });
    let user_content = Rc::new(UserContentManager::new(&servo));
    user_content.add_script(Rc::new(UserScript::new(consent::user_script(), None)));
    let webview = WebViewBuilder::new(&servo, rendering_context.clone())
        .delegate(delegate.clone())
        .user_content_manager(user_content.clone())
        .url(page.url.clone())
        .build();

    let session = Session { servo: &servo, webview: &webview, delegate: &delegate, deadline };
    let result = session.extract(page);

    // Shut Servo down completely before the caller removes the profile: the WebView and the user
    // content manager hold the last other handles, and dropping the last `Servo` handle waits
    // until the constellation reports that every pipeline, storage and network thread has exited.
    drop(webview);
    drop(user_content);
    drop(delegate);
    drop(servo);
    drop(rendering_context);
    result
}

struct Session<'a> {
    servo: &'a Servo,
    webview: &'a WebView,
    delegate: &'a Delegate,
    deadline: Instant,
}

impl Session<'_> {
    fn extract(&self, page: &PageLoad) -> Result<Loaded, EngineError> {
        self.spin_until(self.deadline, &|| self.delegate.load_complete.get())?;
        self.settle_consent()?;

        let json = match self.evaluate(format!("{READABILITY_JS}\n{EXTRACT_JS}"), self.deadline)? {
            JSValue::String(json) => json,
            other => return Err(EngineError::Engine(format!("extract.js returned {other:?}"))),
        };
        let mut extracted: Extracted =
            serde_json::from_str(&json).map_err(|e| EngineError::Engine(format!("extract.js output: {e}")))?;
        classify(&mut extracted)?;

        let picture = match (&extracted.picture_url, page.want_picture) {
            (Some(url), true) => self.download_picture(url, page.max_picture_bytes),
            _ => None,
        };
        Ok(Loaded { extracted, picture })
    }

    /// Lets autoconsent find and answer a banner, then waits briefly for a navigation that the
    /// answer triggered.
    fn settle_consent(&self) -> Result<(), EngineError> {
        let navigations = self.delegate.navigations.get();
        let started = Instant::now();
        let budget = self.deadline.min(started + CONSENT_BUDGET);
        let last = loop {
            let state = match self.evaluate(consent::STATE_JS, budget) {
                Ok(JSValue::String(json)) => serde_json::from_str::<consent::ConsentState>(&json).ok(),
                // No autoconsent in this document, a navigation interrupted the poll, or time is up.
                _ => None,
            };
            match state {
                Some(state) if !state.settled(started.elapsed()) && Instant::now() < budget => self.wait(POLL_INTERVAL),
                last => break last,
            }
        };
        eprintln!("juttusihti-browser: consent {last:?} after {} ms", started.elapsed().as_millis());
        if self.delegate.navigations.get() != navigations || !self.delegate.load_complete.get() {
            let wait = self.deadline.min(Instant::now() + CONSENT_NAVIGATION_WAIT);
            match self.spin_until(wait, &|| self.delegate.load_complete.get()) {
                // The navigation did not finish in its time: extract what the page shows now.
                Err(EngineError::Timeout) if Instant::now() < self.deadline => {}
                other => other?,
            }
        }
        Ok(())
    }

    /// Downloads the picture with an in-page `fetch()`, so it goes through the same session and
    /// blocker as the page. Picture hosts on another origin usually refuse a script that reads
    /// the bytes (CORS), so then the picture is opened as a page of its own in the same session
    /// (the article is already extracted) and read from there, where it is same-origin. Any
    /// failure means no picture.
    fn download_picture(&self, url: &str, max_bytes: usize) -> Option<Vec<u8>> {
        let page_url = self.delegate.page_url.borrow().clone().map(String::from).unwrap_or_default();
        if self.delegate.blocker.should_block(url, &page_url, ResourceKind::Image) {
            return None;
        }
        let budget = self.deadline.min(Instant::now() + PICTURE_BUDGET);
        match self.fetch_in_page(url, max_bytes, budget) {
            Ok(bytes) => return Some(bytes),
            Err(PictureError::Unreadable(error)) => {
                eprintln!(
                    "juttusihti-browser: picture unreadable from the article page ({error}); opening it directly"
                );
            }
            Err(PictureError::Failed(error)) => {
                eprintln!("juttusihti-browser: picture not downloaded: {error}");
                return None;
            }
        }

        let picture_url = Url::parse(url).ok()?;
        let navigations = self.delegate.navigations.get();
        self.webview.load(picture_url);
        self.spin_until(budget, &|| {
            self.delegate.navigations.get() != navigations && self.delegate.load_complete.get()
        })
        .ok()?;
        match self.fetch_in_page(url, max_bytes, budget) {
            Ok(bytes) => Some(bytes),
            Err(PictureError::Unreadable(error) | PictureError::Failed(error)) => {
                eprintln!("juttusihti-browser: picture not downloaded: {error}");
                None
            }
        }
    }

    /// Runs the in-page picture download in the current document and waits for its bytes.
    fn fetch_in_page(&self, url: &str, max_bytes: usize, budget: Instant) -> Result<Vec<u8>, PictureError> {
        let failed = |e: &dyn std::fmt::Display| PictureError::Failed(e.to_string());
        let url_literal = serde_json::to_string(url).map_err(|e| failed(&e))?;
        self.evaluate(format!("{PICTURE_FETCH_JS}({url_literal}, {max_bytes})"), budget).map_err(|e| failed(&e))?;
        loop {
            match self.evaluate(PICTURE_POLL_JS, budget) {
                Ok(JSValue::String(json)) => {
                    let done: PictureDone = serde_json::from_str(&json).map_err(|e| failed(&e))?;
                    return match (done.data, done.error) {
                        (Some(data), _) => {
                            let bytes =
                                base64::engine::general_purpose::STANDARD.decode(data).map_err(|e| failed(&e))?;
                            if bytes.len() > max_bytes {
                                return Err(failed(&"picture too large"));
                            }
                            Ok(bytes)
                        }
                        // Servo reports a refused cross-origin read as a CORS network error.
                        (None, Some(error)) if error.contains("CORS") => Err(PictureError::Unreadable(error)),
                        (None, error) => Err(PictureError::Failed(error.unwrap_or_default())),
                    };
                }
                Ok(_) if Instant::now() < budget => self.wait(POLL_INTERVAL),
                Ok(_) => return Err(failed(&"timed out")),
                Err(e) => return Err(failed(&e)),
            }
        }
    }

    /// Evaluates `script` in the top frame and waits for its completion value.
    fn evaluate(&self, script: impl ToString, until: Instant) -> Result<JSValue, EngineError> {
        let result = Rc::new(RefCell::new(None));
        let sink = result.clone();
        self.webview.evaluate_javascript(script, move |value| {
            *sink.borrow_mut() = Some(value);
        });
        self.spin_until(until, &|| result.borrow().is_some())?;
        let value = result.borrow_mut().take().expect("spin_until waited for the result");
        value.map_err(|e| EngineError::Engine(format!("script failed: {e:?}")))
    }

    /// Keeps Servo running for `duration` (bounded by the deadline).
    fn wait(&self, duration: Duration) {
        let until = self.deadline.min(Instant::now() + duration);
        let _ = self.spin_until(until, &|| false);
    }

    /// Spins Servo until `done` returns true, the page crashes, or `until` passes.
    fn spin_until(&self, until: Instant, done: &dyn Fn() -> bool) -> Result<(), EngineError> {
        while !done() {
            if let Some(reason) = self.delegate.crashed.borrow().as_ref() {
                return Err(EngineError::Engine(format!("page crashed: {reason}")));
            }
            if Instant::now() >= until {
                return Err(EngineError::Timeout);
            }
            self.servo.spin_event_loop();
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    }
}

enum PictureError {
    /// The picture host does not let the article page's scripts read it.
    Unreadable(String),
    Failed(String),
}

#[derive(Deserialize)]
struct PictureDone {
    data: Option<String>,
    error: Option<String>,
}

/// Starts the picture download; called as `(url, maxBytes)`. Stops reading past `maxBytes`.
const PICTURE_FETCH_JS: &str = r#"(function (url, max) {
  var slot = { done: false, data: null, error: null };
  window.__juttusihtiPicture = slot;
  function tooLarge() { throw new Error("picture larger than " + max + " bytes"); }
  function readAll(response) {
    if (!response.body || !response.body.getReader) {
      return response.arrayBuffer().then(function (buf) { return new Uint8Array(buf); });
    }
    var reader = response.body.getReader();
    var chunks = [];
    var total = 0;
    function next() {
      return reader.read().then(function (step) {
        if (step.done) {
          var all = new Uint8Array(total);
          var at = 0;
          chunks.forEach(function (c) { all.set(c, at); at += c.length; });
          return all;
        }
        total += step.value.length;
        if (total > max) { reader.cancel(); tooLarge(); }
        chunks.push(step.value);
        return next();
      });
    }
    return next();
  }
  fetch(url)
    .then(function (response) {
      if (!response.ok) throw new Error("HTTP " + response.status);
      var length = parseInt(response.headers.get("content-length"), 10);
      if (length > max) tooLarge();
      return readAll(response);
    })
    .then(function (bytes) {
      var binary = "";
      for (var i = 0; i < bytes.length; i += 0x8000) {
        binary += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
      }
      slot.data = btoa(binary);
    })
    .catch(function (e) { slot.error = String(e); })
    .then(function () { slot.done = true; });
  return true;
})"#;

/// `null` while the download runs, then `{"data": base64 | null, "error": string | null}`.
const PICTURE_POLL_JS: &str = "(function () { var s = window.__juttusihtiPicture; \
     return s && s.done ? JSON.stringify({ data: s.data, error: s.error }) : null; })()";

/// Runs `create` while a throwaway GL context is current on this thread.
///
/// Servo 0.7's `SoftwareRenderingContext::new` loads its GL function pointers and reads
/// `GL_VERSION` before it makes its own context current. On Windows (WGL) nothing is current at
/// that point, so the load panics. A context from the same software adapter, current only while
/// Servo's is created, gives it valid pointers; Servo's context is current when `create` returns.
fn with_current_gl_context<T>(create: impl FnOnce() -> T) -> Result<T, EngineError> {
    use surfman::{ContextAttributeFlags, ContextAttributes, GLApi, GLVersion};

    let gl_error = |e: surfman::Error| EngineError::Engine(format!("GL bootstrap: {e:?}"));
    let connection = surfman::Connection::new().map_err(gl_error)?;
    let adapter = connection.create_software_adapter().map_err(gl_error)?;
    let device = connection.create_device(&adapter).map_err(gl_error)?;
    let version = match connection.gl_api() {
        GLApi::GLES => GLVersion { major: 3, minor: 0 },
        GLApi::GL => GLVersion { major: 3, minor: 2 },
    };
    let descriptor = device
        .create_context_descriptor(&ContextAttributes {
            flags: ContextAttributeFlags::ALPHA | ContextAttributeFlags::DEPTH | ContextAttributeFlags::STENCIL,
            version,
        })
        .map_err(gl_error)?;
    let mut context = device.create_context(&descriptor, None).map_err(gl_error)?;
    device.make_context_current(&context).map_err(gl_error)?;
    let created = create();
    let _ = device.destroy_context(&mut context);
    Ok(created)
}

/// Article text with paragraph breaks kept and site controls dropped.
pub fn article_text(raw: &str) -> String {
    juttusihti_article::normalize_text(raw)
        .split("\n\n")
        .filter(|paragraph| !UI_NOISE.contains(&paragraph.trim().to_lowercase().as_str()))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Turns a refused or article-less page into an error; cleans the text otherwise.
fn classify(extracted: &mut Extracted) -> Result<(), EngineError> {
    if let Some(status) = extracted.main_status.filter(|s| *s >= 400) {
        return Err(EngineError::Refused(status));
    }
    extracted.text = article_text(&extracted.text);
    if extracted.text.chars().count() < MIN_ARTICLE_CHARS {
        return Err(EngineError::NoArticle);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extracted(status: Option<u16>, text: &str) -> Extracted {
        Extracted {
            main_status: status,
            title: String::new(),
            text: text.to_string(),
            picture_url: None,
            paywalled: false,
        }
    }

    #[test]
    fn status_400_or_more_is_refused() {
        let mut page = extracted(Some(403), &"x".repeat(500));
        assert!(matches!(classify(&mut page), Err(EngineError::Refused(403))));
    }

    #[test]
    fn short_text_is_no_article() {
        let mut page = extracted(Some(200), &format!("  {}  ", "x".repeat(199)));
        assert!(matches!(classify(&mut page), Err(EngineError::NoArticle)));
    }

    #[test]
    fn long_text_is_kept_trimmed() {
        let mut page = extracted(None, &format!("\n {} \n", "x".repeat(200)));
        classify(&mut page).expect("article");
        assert_eq!(page.text, "x".repeat(200));
    }

    #[test]
    fn article_text_keeps_paragraphs_and_drops_ui_labels() {
        let raw = "Lue tiivistelmä\n\nFirst  paragraph.\n\n\n  Second paragraph. \n\nLuitko jo nämä?\n\nMAINOS\n\nTilaajille\n\nThird.";
        assert_eq!(article_text(raw), "First  paragraph.\n\nSecond paragraph.\n\nThird.");
    }

    #[test]
    fn media_and_frames_map_to_filter_types() {
        assert_eq!(resource_kind(Destination::Video), ResourceKind::Media);
        assert_eq!(resource_kind(Destination::IFrame), ResourceKind::Subdocument);
        assert_eq!(resource_kind(Destination::None), ResourceKind::Xhr);
        assert_eq!(resource_kind(Destination::ServiceWorker), ResourceKind::Script);
    }
}
