# Vendored assets

Pinned copies bundled into `juttusihti-browser` at build time (`include_str!`). Nothing here is
downloaded at run time; refresh them only with a Juttusihti release, and update this file when you do.
All files are unmodified copies of the upstream files.

| File | What | Version | Source | Licence |
|------|------|---------|--------|---------|
| `Readability.js` | Mozilla Readability | 0.6.0 | https://raw.githubusercontent.com/mozilla/readability/0.6.0/Readability.js | Apache-2.0 |
| `easylist.txt` | EasyList | `! Version: 202611062013` (commit `4e3005b0331c271bae3094cfcdb6a403d6998a81`) | https://easylist.to/easylist/easylist.txt | CC BY-SA 3.0 (dual GPLv3+ / CC BY-SA 3.0; see below) |
| `easyprivacy.txt` | EasyPrivacy | `! Version: 202611062013` (commit `4e3005b0331c271bae3094cfcdb6a403d6998a81`) | https://easylist.to/easylist/easyprivacy.txt | CC BY-SA 3.0 (dual GPLv3+ / CC BY-SA 3.0; see below) |
| `autoconsent.js` | DuckDuckGo autoconsent, `dist/autoconsent.playwright.js` (in-page bundle, no rules) | `@duckduckgo/autoconsent` 18.0.0 | https://registry.npmjs.org/@duckduckgo/autoconsent/-/autoconsent-18.0.0.tgz | MPL-2.0 |
| `autoconsent-rules.json` | DuckDuckGo autoconsent bundled rule set, `rules/rules.json` (full rules; the compact set carries no opt-in steps, and Juttusihti accepts banners) | `@duckduckgo/autoconsent` 18.0.0 | same package as above | MPL-2.0 |
| `extract.js` | Juttusihti's own extraction script | — | this repository | MIT OR Apache-2.0 |
| `consent-extra-rules.json` | Juttusihti's own consent rule, in autoconsent's rule format: accepts the free choice of Sourcepoint consent-or-pay walls (never a paid, subscribe or login choice) | — | this repository | MIT OR Apache-2.0 |

SHA-256 of the vendored copies:

```
ca0a580473d71a1f01edcba8cf7e6337d634f5d31f8bcf244faf3dc98b11c3c4  easylist.txt
8aba04de2d50a394f1d28f43fc3e29fd7a510683a519b763cea47d07b0ab2854  easyprivacy.txt
08698eb12abc227ff03a3503cec101c97904cc3bcc28075ee1f7cf70615a7f2d  autoconsent.js
1d70481c72c5667b50b96d007ca2f1da28bec9854dc53cf2b8695b9e3c6dfccf  autoconsent-rules.json
```

## Licence decisions

Juttusihti itself is MIT OR Apache-2.0. Each vendored file keeps its own licence; none of them
changes the licence of the app's own code.

- **EasyList and EasyPrivacy** are dual licensed under the GNU GPL version 3 (or later) and
  Creative Commons Attribution-ShareAlike 3.0 Unported
  (https://easylist.to/pages/licence.html). Juttusihti uses them under **CC BY-SA 3.0**, which
  allows redistribution of the lists as data alongside software under any licence. Obligations:
  credit the source, link the licence, and share any modified version of the lists under the same
  licence. We ship them unmodified.
  Attribution: *EasyList and EasyPrivacy, by The EasyList authors (https://easylist.to/),
  licensed under CC BY-SA 3.0 (https://creativecommons.org/licenses/by-sa/3.0/).*
- **autoconsent** (code and rules) is MPL-2.0, a file-level copyleft: the files stay under
  MPL-2.0 and their source must be available to recipients; larger works that include them may
  use any licence. The files are shipped unmodified, in source form (embedded as text in the
  helper binary), and their source is also available at the URL above. Juttusihti's driver for
  them (`src/consent.rs`) is its own code.
- **Readability.js** is Apache-2.0; ship its licence notice with the app.

This file covers only the vendored assets above. The Rust crates the helper is built from (Servo
and adblock-rust by Brave, both MPL-2.0, and others) declare their licences in their Cargo
metadata; `cargo metadata --manifest-path crates/browser/Cargo.toml` (or a tool such as
`cargo about`) lists them. Their source is on crates.io.
