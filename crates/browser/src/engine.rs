//! Loads one page in a headless Servo and extracts the article from it.
//!
//! The user agent is exactly the one Servo ships with and is never changed (see the design's
//! browser identity rule).

use std::cell::{Cell, RefCell};
use std::path::Path;
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use servo::{
    JSValue, LoadStatus, Opts, Preferences, RenderingContext, ServoBuilder, SoftwareRenderingContext, WebView,
    WebViewBuilder, WebViewDelegate,
};
use url::Url;

const READABILITY_JS: &str = include_str!("../assets/Readability.js");
const EXTRACT_JS: &str = include_str!("../assets/extract.js");

/// Shorter Readability text than this means the page had no article.
const MIN_ARTICLE_CHARS: usize = 200;

/// The viewport the page is laid out in. Nothing is shown; it only shapes layout.
const VIEWPORT: dpi::PhysicalSize<u32> = dpi::PhysicalSize { width: 1280, height: 900 };

/// What was extracted from a loaded article page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Extracted {
    /// HTTP status of the main document, when the engine exposes it (Servo 0.7 does not).
    pub main_status: Option<u16>,
    pub title: String,
    pub text: String,
    pub html_lang: Option<String>,
    pub picture_url: Option<String>,
    /// The page's JSON-LD says the article is not free (`isAccessibleForFree: false`).
    pub paywalled: bool,
    /// Names of the cookies visible to the page (`document.cookie`).
    pub cookie_names: Vec<String>,
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

#[derive(Default)]
struct Delegate {
    load_complete: Cell<bool>,
    crashed: RefCell<Option<String>>,
}

impl WebViewDelegate for Delegate {
    fn notify_load_status_changed(&self, _webview: WebView, status: LoadStatus) {
        if status == LoadStatus::Complete {
            self.load_complete.set(true);
        }
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, _backtrace: Option<String>) {
        *self.crashed.borrow_mut() = Some(reason);
    }
}

/// Loads `url` in a fresh Servo whose config directory is `profile_dir`, waits for the load to
/// complete, and extracts the article text, main picture, paywall flag and cookie names.
pub fn load_and_extract(url: &Url, profile_dir: &Path, timeout: Duration) -> Result<Extracted, EngineError> {
    let deadline = Instant::now() + timeout;

    let rendering_context = Rc::new(with_current_gl_context(|| {
        SoftwareRenderingContext::new(VIEWPORT).map_err(|e| EngineError::Engine(format!("rendering context: {e:?}")))
    })??);
    rendering_context.make_current().map_err(|e| EngineError::Engine(format!("rendering context: {e:?}")))?;

    let opts = Opts { config_dir: Some(profile_dir.to_path_buf()), temporary_storage: true, ..Opts::default() };
    // IS and HS (Next.js) crash without IntersectionObserver, which Servo 0.7 ships behind this
    // experimental-feature pref. It only enables a standard web API; nothing about the browser's
    // identity changes.
    let preferences = Preferences { dom_intersection_observer_enabled: true, ..Preferences::default() };
    let servo = ServoBuilder::default().opts(opts).preferences(preferences).build();

    let delegate = Rc::new(Delegate::default());
    let webview = WebViewBuilder::new(&servo, rendering_context).delegate(delegate.clone()).url(url.clone()).build();

    // Spins Servo until `done` returns true, the page crashes, or the deadline passes.
    let spin_until = |done: &dyn Fn() -> bool| -> Result<(), EngineError> {
        while !done() {
            if let Some(reason) = delegate.crashed.borrow().as_ref() {
                return Err(EngineError::Engine(format!("page crashed: {reason}")));
            }
            if Instant::now() >= deadline {
                return Err(EngineError::Timeout);
            }
            servo.spin_event_loop();
            thread::sleep(Duration::from_millis(1));
        }
        Ok(())
    };

    spin_until(&|| delegate.load_complete.get())?;

    let result = Rc::new(RefCell::new(None));
    let sink = result.clone();
    webview.evaluate_javascript(format!("{READABILITY_JS}\n{EXTRACT_JS}"), move |value| {
        *sink.borrow_mut() = Some(value);
    });
    spin_until(&|| result.borrow().is_some())?;

    let value = result.borrow_mut().take().expect("spin_until waited for the result");
    let json = match value {
        Ok(JSValue::String(json)) => json,
        Ok(other) => {
            return Err(EngineError::Engine(format!("extract.js returned {other:?}")));
        }
        Err(e) => return Err(EngineError::Engine(format!("extract.js failed: {e:?}"))),
    };
    let mut extracted: Extracted =
        serde_json::from_str(&json).map_err(|e| EngineError::Engine(format!("extract.js output: {e}")))?;
    classify(&mut extracted)?;
    Ok(extracted)
}

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

/// Turns a refused or article-less page into an error; trims the text otherwise.
fn classify(extracted: &mut Extracted) -> Result<(), EngineError> {
    if let Some(status) = extracted.main_status.filter(|s| *s >= 400) {
        return Err(EngineError::Refused(status));
    }
    extracted.text = extracted.text.trim().to_string();
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
            html_lang: None,
            picture_url: None,
            paywalled: false,
            cookie_names: vec![],
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
}
