# Contributing to Juttusihti

Thanks for helping. Bug reports, ideas and pull requests are all welcome.

## Reporting bugs and asking for features

Open an issue in [GitHub Issues](https://github.com/tero-k/juttusihti/issues) and pick the
matching template. For a bug, include the Juttusihti version, your OS, and the
steps that reproduce it. Please do **not** report security problems in public issues: see
[SECURITY.md](SECURITY.md).

## Making changes

1. Fork the repository and create a branch from `master`.
2. Build and run it as described in [Build from source](README.md#build-from-source).
3. Before opening a pull request, run the same checks CI runs:

   ```sh
   cargo fmt --all
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace
   ```

4. Keep the [invariants](docs/development.md#invariants-enforced-in-code-and-tests) intact.
5. When you change how something is used, update the in-app Help (`crates/app/src/help.rs`)
   and the [user guide](docs/guide.md), and add a line to [CHANGELOG.md](CHANGELOG.md).

[docs/development.md](docs/development.md) describes the crates, CI, releases and packaging.

Keep pull requests focused on one change, and explain what it does and why.

## License

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion
in this work, as defined in the Apache-2.0 license, shall be dual licensed under MIT OR
Apache-2.0, without any additional terms or conditions.

## Code of conduct

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
