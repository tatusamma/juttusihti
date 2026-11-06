//! Cookie consent banners, answered by DuckDuckGo's autoconsent with its bundled rules.
//!
//! The autoconsent bundle runs as a user script in every frame of the page, from the moment the
//! document's `<head>` exists (so it can also hide a banner before it shows). A small driver plays
//! the part of autoconsent's "background" side: it hands over the configuration and the bundled
//! rules, first asks for an opt-out (reject, "only necessary"), and only when that fails asks for an
//! opt-in. The state of the top frame is kept in `window.__juttusihtiConsent` for the engine to
//! poll. Nothing is downloaded: the rules are pinned in `assets/` (see `assets/LICENSES.md`).

use std::time::Duration;

use serde::Deserialize;

/// How long after the load to keep looking when no consent platform has been recognised.
pub const NO_CMP_GRACE: Duration = Duration::from_millis(2500);

const AUTOCONSENT_JS: &str = include_str!("../assets/autoconsent.js");
const AUTOCONSENT_RULES: &str = include_str!("../assets/autoconsent-rules.json");

/// Runs before the bundle; `__RULES__` is replaced with the bundled compact rule set.
const DRIVER_JS: &str = r#"
// Only web pages get autoconsent. Throwing stops this user script before the bundle runs: in
// Servo 0.7 reading location.ancestorOrigins (autoconsent's top-frame check) panics the script
// thread on documents that were not created by a navigation to an HTML page.
if (!/^https?:$/.test(location.protocol) || !/^text\/html(;|$)/i.test(document.contentType)) {
  throw new Error("autoconsent: not a web page");
}
(function () {
  if (window.__juttusihtiConsent) return;
  var state = { lifecycle: null, detected: null, cmp: null, optOut: null, optIn: null };
  window.__juttusihtiConsent = state;
  var config = {
    enabled: true,
    autoAction: "optOut",
    isMainWorld: true,
    enablePrehide: true,
    enableCosmeticRules: true,
    enableGeneratedRules: true,
    detectRetries: 10,
    logs: { lifecycle: false, rulesteps: false, detectionsteps: false, evals: false, errors: false, messages: false, waits: false }
  };
  var rules = __RULES__;
  function reply(message) {
    setTimeout(function () {
      try { window.autoconsentReceiveMessage(message); } catch (e) {}
    }, 0);
  }
  window.autoconsentSendMessage = function (message) {
    switch (message.type) {
      case "init":
        reply({ type: "initResp", config: config, rules: { autoconsent: [], compact: rules } });
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
        break;
      case "optOutResult":
        state.optOut = !!message.result;
        // No reject choice worked: accept instead, as the design allows when rejecting is impossible.
        if (!message.result) reply({ type: "optIn" });
        break;
      case "optInResult":
        state.optIn = !!message.result;
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
    let driver = DRIVER_JS.replace("__RULES__", AUTOCONSENT_RULES.trim());
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
    pub opt_out: Option<bool>,
    pub opt_in: Option<bool>,
}

impl ConsentState {
    /// Autoconsent has nothing more to do: no banner, or the banner has been answered (rejected,
    /// or accepted after rejecting failed), successfully or not. `waited` is the time since the
    /// page finished loading: with no consent platform recognised by `NO_CMP_GRACE`, there is
    /// none to wait for (autoconsent itself would keep looking for a minute on a quiet page).
    pub fn settled(&self, waited: Duration) -> bool {
        if self.opt_out == Some(true) || self.opt_in.is_some() {
            return true;
        }
        if self.detected.is_none() && self.cmp.is_none() && waited >= NO_CMP_GRACE {
            return true;
        }
        matches!(self.lifecycle.as_deref(), Some("nothingDetected" | "done" | "optInSucceeded" | "optInFailed"))
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
    fn settles_on_no_banner_or_an_answer() {
        assert!(state(r#"{"lifecycle":"nothingDetected","cmp":null,"optOut":null,"optIn":null}"#).settled(SOON));
        assert!(state(r#"{"lifecycle":"optOutSucceeded","cmp":"x","optOut":true,"optIn":null}"#).settled(SOON));
        assert!(state(r#"{"lifecycle":"optInFailed","cmp":"x","optOut":false,"optIn":false}"#).settled(SOON));
    }

    #[test]
    fn failed_opt_out_waits_for_the_opt_in() {
        let failed = state(r#"{"lifecycle":"optOutFailed","detected":"x","cmp":"x","optOut":false,"optIn":null}"#);
        assert!(!failed.settled(SOON));
        assert!(!failed.settled(NO_CMP_GRACE * 2));
    }

    #[test]
    fn stops_looking_soon_when_no_platform_is_recognised() {
        let looking = state(r#"{"lifecycle":"started","detected":null,"cmp":null,"optOut":null,"optIn":null}"#);
        assert!(!looking.settled(SOON));
        assert!(looking.settled(NO_CMP_GRACE));
        let detected =
            state(r#"{"lifecycle":"cmpDetected","detected":"Sourcepoint","cmp":null,"optOut":null,"optIn":null}"#);
        assert!(!detected.settled(NO_CMP_GRACE * 2));
    }

    #[test]
    fn user_script_carries_the_bundled_rules() {
        let script = user_script();
        assert!(!script.contains("__RULES__"));
        assert!(script.contains("autoconsentReceiveMessage"));
        assert!(script.len() > AUTOCONSENT_JS.len() + AUTOCONSENT_RULES.len() / 2);
    }
}
