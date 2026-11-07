//! Cookie consent banners, answered by DuckDuckGo's autoconsent with its bundled rules.
//!
//! The autoconsent bundle runs as a user script in every frame of the page, from the moment the
//! document's `<head>` exists (so it can also hide a banner before it shows). A small driver plays
//! the part of autoconsent's "background" side: it hands over the configuration and the bundled
//! rules with `autoAction: "optIn"`, so a banner is accepted straight away (the user's choice),
//! with no opt-out attempt first. The reader never clicks, and the whole profile, cookies
//! included, is deleted after the article is extracted.
//!
//! Besides autoconsent's rules, Juttusihti adds its own (`assets/consent-extra-rules.json`): on a
//! Sourcepoint "consent or pay" wall, which autoconsent deliberately skips, it clicks only the free
//! accept-all choice, never a paid, subscribe or login one.
//!
//! The state of the top frame is kept in `window.__juttusihtiConsent` for the engine to poll.
//! Frames (Sourcepoint shows its message in an iframe) report the banners they found and their
//! answers to the top frame, so the engine waits for an answer given inside a frame too.
//! Nothing is downloaded: the rules are pinned in `assets/` (see `assets/LICENSES.md`).

use std::time::Duration;

use serde::Deserialize;

/// How long after the load to keep looking when no consent platform has been recognised.
pub const NO_CMP_GRACE: Duration = Duration::from_millis(2500);

/// How long to wait for a frame to answer a banner the top frame saw but could not answer.
pub const FRAME_ANSWER_WAIT: Duration = Duration::from_secs(5);

const AUTOCONSENT_JS: &str = include_str!("../assets/autoconsent.js");
const AUTOCONSENT_RULES: &str = include_str!("../assets/autoconsent-rules.json");
/// Juttusihti's own rules, in autoconsent's rule format, loaded after the bundled ones.
const EXTRA_RULES: &str = include_str!("../assets/consent-extra-rules.json");

/// Runs before the bundle; `__RULES__` is replaced with the bundled rule set and `__EXTRA_RULES__`
/// with Juttusihti's own. The bundled set is the full `rules/rules.json`, not
/// `compact-rules.json`: autoconsent's compact rules carry no opt-in steps.
const DRIVER_JS: &str = r#"
// Only web pages get autoconsent. Throwing stops this user script before the bundle runs: in
// Servo 0.7 reading location.ancestorOrigins (autoconsent's top-frame check) panics the script
// thread on documents that were not created by a navigation to an HTML page.
if (!/^https?:$/.test(location.protocol) || !/^text\/html(;|$)/i.test(document.contentType)) {
  throw new Error("autoconsent: not a web page");
}
(function () {
  if (window.__juttusihtiConsent) return;
  var isTop = window.top === window;
  var state = { lifecycle: null, detected: null, cmp: null, optIn: null, framePopups: 0, frameAnswers: [] };
  window.__juttusihtiConsent = state;
  // Frames tell the top frame about the banners they found and how they answered them.
  function toTop(type, message) {
    if (isTop) return;
    try {
      window.top.postMessage({ juttusihtiConsentFrame: { type: type, result: !!message.result } }, "*");
    } catch (e) {}
  }
  if (isTop) {
    window.addEventListener("message", function (event) {
      var report = event.data && event.data.juttusihtiConsentFrame;
      if (!report) return;
      if (report.type === "popupFound") state.framePopups += 1;
      if (report.type === "optInResult") state.frameAnswers.push(report.result);
    });
  }
  var config = {
    enabled: true,
    autoAction: "optIn",
    isMainWorld: true,
    enablePrehide: true,
    enableCosmeticRules: true,
    enableGeneratedRules: true,
    detectRetries: 10,
    logs: { lifecycle: false, rulesteps: false, detectionsteps: false, evals: false, errors: false, messages: false, waits: false }
  };
  var rules = __RULES__;
  var extraRules = __EXTRA_RULES__;
  function reply(message) {
    setTimeout(function () {
      try { window.autoconsentReceiveMessage(message); } catch (e) {}
    }, 0);
  }
  window.autoconsentSendMessage = function (message) {
    switch (message.type) {
      case "init":
        reply({ type: "initResp", config: config, rules: { autoconsent: rules.autoconsent.concat(extraRules.autoconsent) } });
        break;
      case "eval":
        // Only the bundled rules send code here (normally evaluated inline, as isMainWorld is set).
        var result = false;
        try { result = !!(0, eval)(message.code); } catch (e) {}
        reply({ type: "evalResp", id: message.id, result: result });
        break;
      case "report":
        state.lifecycle = message.state.lifecycle;
        break;
      case "cmpDetected":
        state.detected = message.cmp;
        break;
      case "popupFound":
        state.cmp = message.cmp;
        toTop("popupFound", message);
        break;
      case "optInResult":
        state.optIn = !!message.result;
        toTop("optInResult", message);
        break;
    }
    return Promise.resolve();
  };
})();
"#;

/// Evaluated in the top frame; returns the driver's state as JSON, or `null` when autoconsent
/// never ran (e.g. a document without `<head>`).
pub const STATE_JS: &str =
    "(function () { var s = window.__juttusihtiConsent; return s ? JSON.stringify(s) : null; })()";

/// The user script: driver, then the autoconsent bundle.
pub fn user_script() -> String {
    let driver =
        DRIVER_JS.replace("__RULES__", AUTOCONSENT_RULES.trim()).replace("__EXTRA_RULES__", EXTRA_RULES.trim());
    format!("{driver}\n{AUTOCONSENT_JS}")
}

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsentState {
    pub lifecycle: Option<String>,
    /// A consent platform was recognised on the page (its banner may not be open).
    pub detected: Option<String>,
    /// The consent platform whose banner was found open.
    pub cmp: Option<String>,
    /// Result of accepting the banner.
    pub opt_in: Option<bool>,
    /// Banners found open in frames, and the frames' answers so far.
    #[serde(default)]
    pub frame_popups: u32,
    #[serde(default)]
    pub frame_answers: Vec<bool>,
}

impl ConsentState {
    /// Autoconsent has nothing more to do: no banner, or the banner has been accepted (in the top
    /// frame or in a frame), successfully or not. `waited` is the time since the page finished
    /// loading: with no consent platform recognised by `NO_CMP_GRACE`, there is none to wait for
    /// (autoconsent itself would keep looking for a minute on a quiet page).
    pub fn settled(&self, waited: Duration) -> bool {
        // A frame found a banner and is still answering it.
        if self.frame_popups as usize > self.frame_answers.len() {
            return false;
        }
        if self.opt_in == Some(true) || self.frame_answers.contains(&true) {
            return true;
        }
        let top_done = self.opt_in.is_some()
            || matches!(self.lifecycle.as_deref(), Some("nothingDetected" | "done" | "optInSucceeded" | "optInFailed"))
            || (self.detected.is_none() && self.cmp.is_none() && waited >= NO_CMP_GRACE);
        if !top_done {
            return false;
        }
        // The top frame saw a banner it could not accept (Sourcepoint's companion rule only
        // checks): its message lives in a frame, which may still answer.
        if self.cmp.is_some() {
            return waited >= FRAME_ANSWER_WAIT;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(json: &str) -> ConsentState {
        serde_json::from_str(json).unwrap()
    }

    const SOON: Duration = Duration::from_millis(100);

    #[test]
    fn settles_on_no_banner_or_an_accept() {
        assert!(state(r#"{"lifecycle":"nothingDetected","cmp":null,"optIn":null}"#).settled(SOON));
        assert!(state(r#"{"lifecycle":"optInSucceeded","cmp":"x","optIn":true}"#).settled(SOON));
        // A banner the top frame could not accept: frames get time to answer it first.
        let failed = state(r#"{"lifecycle":"optInFailed","cmp":"x","optIn":false}"#);
        assert!(!failed.settled(SOON));
        assert!(failed.settled(FRAME_ANSWER_WAIT));
    }

    #[test]
    fn open_banner_waits_for_the_accept() {
        let running = state(r#"{"lifecycle":"runningOptIn","detected":"x","cmp":"x","optIn":null}"#);
        assert!(!running.settled(SOON));
        assert!(!running.settled(NO_CMP_GRACE * 2));
    }

    #[test]
    fn driver_accepts_without_opting_out_first() {
        assert!(DRIVER_JS.contains(r#"autoAction: "optIn""#));
        assert!(!DRIVER_JS.contains("optOut"));
    }

    #[test]
    fn stops_looking_soon_when_no_platform_is_recognised() {
        let looking = state(r#"{"lifecycle":"started","detected":null,"cmp":null,"optIn":null}"#);
        assert!(!looking.settled(SOON));
        assert!(looking.settled(NO_CMP_GRACE));
        let detected = state(r#"{"lifecycle":"cmpDetected","detected":"Sourcepoint","cmp":null,"optIn":null}"#);
        assert!(!detected.settled(NO_CMP_GRACE * 2));
    }

    #[test]
    fn waits_for_a_frame_to_answer() {
        let top_failed =
            r#""lifecycle":"optInFailed","detected":"sourcepoint-top","cmp":"sourcepoint-top","optIn":false"#;
        let answering = state(&format!(r#"{{{top_failed},"framePopups":1,"frameAnswers":[]}}"#));
        assert!(!answering.settled(FRAME_ANSWER_WAIT * 2));
        let answered = state(&format!(r#"{{{top_failed},"framePopups":1,"frameAnswers":[true]}}"#));
        assert!(answered.settled(SOON));
        let nothing_yet = state(&format!(r#"{{{top_failed},"framePopups":0,"frameAnswers":[]}}"#));
        assert!(!nothing_yet.settled(SOON));
        assert!(nothing_yet.settled(FRAME_ANSWER_WAIT));
    }

    #[test]
    fn extra_rule_only_accepts_and_is_frame_only() {
        let rules: serde_json::Value = serde_json::from_str(EXTRA_RULES).unwrap();
        let rule = &rules["autoconsent"][0];
        assert_eq!(rule["runContext"]["main"], false);
        assert_eq!(rule["runContext"]["frame"], true);
        assert!(rule["runContext"]["urlPattern"].as_str().unwrap().contains("message_id="));
        let click = rule["optIn"][0]["waitForThenClick"].as_str().unwrap();
        assert!(click.starts_with("xpath//"));
        for refused in ["consent-pay", "€", "tilaa", "subscri", "kirjaudu", "sign in", "mainok"] {
            assert!(click.contains(&format!("'{refused}'")) || click.contains(&format!(" {refused} ")), "{refused}");
        }
        // Its opt-out can never succeed: the selector matches nothing a page would have.
        assert_eq!(rule["optOut"], serde_json::json!([{ "exists": "#juttusihti-this-rule-never-opts-out" }]));
    }

    #[test]
    fn user_script_carries_the_bundled_rules() {
        let script = user_script();
        assert!(!script.contains("__RULES__") && !script.contains("__EXTRA_RULES__"));
        assert!(script.contains("juttusihti-sourcepoint-accept-free"));
        assert!(script.contains("autoconsentReceiveMessage"));
        assert!(script.len() > AUTOCONSENT_JS.len() + AUTOCONSENT_RULES.len() / 2);
    }
}
