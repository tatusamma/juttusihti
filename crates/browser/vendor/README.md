# Patched crates

Servo 0.8 pins exact versions of two crates that have security advisories, and of one with a
bug that shows on screen, so `cargo update` cannot pick up fixes. These copies replace them
through `[patch.crates-io]` in `../Cargo.toml`. Each one is the upstream crate with the
smallest change that makes the fix fit; everything not listed below is an unmodified copy of
the published crate.

| Directory | Replaces | Contents | Fixes | Licence |
|---|---|---|---|---|
| `rsa` | `rsa 0.10.0-rc.19` (pinned by `servo-script-webcrypto`) | The released `rsa 0.10.0` | RUSTSEC-2023-0071 (Marvin attack) | MIT OR Apache-2.0 |
| `servo-script-webcrypto` | `servo-script-webcrypto 0.8.0` | The published 0.8.0 with one line adapted to `rsa 0.10.0` | — (needed by `rsa`) | MPL-2.0 |
| `libsqlite3-sys` | `libsqlite3-sys 0.36.0` (required by `rusqlite 0.38`, used by `servo-net` and `servo-storage`) | 0.36.0 with the bundled SQLite 3.54.1 from `libsqlite3-sys 0.39.1` | RUSTSEC-2026-0459 (CVE-2026-119375) | MIT (SQLite: public domain) |
| `surfman` | `surfman 0.14.0` (required as `0.14.0` by Servo) | The published 0.14.0 with its WGL setup window kept hidden | A window that flashes and takes the focus at the start of every fetch on Windows (no fixed release) | MIT OR Apache-2.0 OR MPL-2.0 |

## Changes from upstream

**`rsa`**: `src/` is `rsa 0.10.0` unchanged. In `Cargo.toml`:
- `version` is `0.10.0-rc.19`, so it satisfies Servo's `=0.10.0-rc.19` requirement.
- `pkcs1v15` is in the default features. 0.10.0 made PKCS#1 v1.5 opt-in, and Servo's
  WebCrypto uses it for RSASSA-PKCS1-v1_5 without asking for the feature.
- The `[[test]]`, `[[bench]]` and `[dev-dependencies]` tables are removed, because the test and
  bench files are not vendored.

**`servo-script-webcrypto`**: `subtlecrypto/rsa_common.rs` line 869 clones the prime as
`BoxedUint::clone(p_i)` instead of `p_i.clone()`, because `rsa 0.10.0`'s
`PrivateKeyParts::primes()` returns `Odd<BoxedUint>` instead of `BoxedUint`. The line exports
the extra primes of a multi-prime key as JWK. Its build warns about the deprecated
`RsaPrivateKey`/`RsaPublicKey` aliases, which 0.10.0 keeps working.

**`libsqlite3-sys`**: `sqlite3/` (`sqlite3.c`, `sqlite3.h`, `sqlite3ext.h`, the prebuilt
`bindgen_bundled_version*.rs` bindings and `wasm32-wasi-vfs.c`) comes from `libsqlite3-sys
0.39.1`. 0.39.1's bindings only add functions, so `rusqlite 0.38` builds against them unchanged.
`build.rs` and the rest are 0.36.0's. The `sqlcipher/` folder is left out: it holds an older
SQLite and is only used by the `bundled-sqlcipher` feature, which nothing here enables.

```
0c657666d819f261cc4007526ad57e1669de9949df7a8ea847aa284a2bf0ee52  libsqlite3-sys/sqlite3/sqlite3.c
d6c84557261893439b341d2e6c35187ae3ea24fbfd843ad94616f72d89242b8c  libsqlite3-sys/sqlite3/sqlite3.h
```

**`surfman`**: `src/wgl/context.rs` creates its `SurfmanFalseWindow` (a temporary window for
loading the WGL extension functions) without `WS_VISIBLE`. Upstream it is a visible 640×480
window at the top-left corner for a few milliseconds, which Windows also makes the foreground
window. A pixel format and a GL context need no visible window: surfman's own
`SurfmanHiddenWindow` is never shown either. The `[[example]]` and `[dev-dependencies]` tables
are removed from `Cargo.toml`, with the example files.

## Keeping them up to date

`cargo audit` skips path crates, so it does not see these copies. When an advisory names
`rsa`, `libsqlite3-sys`/SQLite or `servo-script-webcrypto`, check it against the versions above
by hand.

When Servo is updated, check whether each patch is still needed:
- Remove the `rsa` and `servo-script-webcrypto` patches once Servo depends on `rsa` 0.10.0 or
  later.
- Remove the `libsqlite3-sys` patch once Servo depends on a `rusqlite` that uses
  `libsqlite3-sys` 0.39 or later.
- Remove the `surfman` patch once a surfman release creates that window hidden. Check with a
  window-event log (`SetWinEventHook` for `EVENT_OBJECT_SHOW` and `EVENT_SYSTEM_FOREGROUND`)
  around one helper run.

If a patch is still needed, rebuild its copy from the new upstream crates with the same changes.

To check a copy against upstream, unpack the published crate (`.crate` files are gzipped tars
in Cargo's `registry/cache`) and run `diff -r` against the directory here.
