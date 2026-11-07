# Releasing

Linux packages are built on GitHub. Windows installers are built and signed on the
maintainer's machine, because the code-signing certificate never leaves it. Both end up on one
**draft** GitHub Release, which is published by hand.

| Where | What | How |
| --- | --- | --- |
| GitHub Actions (`release.yml`) | `.deb`, `.rpm`, AppImage, `x86_64-linux.tar.gz`, `SHA256SUMS-linux.txt`, build provenance attestations | on a pushed `v*` tag |
| Maintainer's Windows machine | signed NSIS installer, signed portable `x86_64-windows.zip`, `SHA256SUMS-windows.txt` | `packaging/release-windows.ps1` |

## Prerequisites (Windows machine)

- Rust stable with MSVC (VS 2022 Build Tools), as for any build.
- `cargo install cargo-packager --version 0.11.8 --locked`
- The Windows SDK's `signtool.exe` and a code-signing certificate in the certificate store (a
  hardware token or a cloud HSM is fine), or another signing tool usable as `-SignCommand`.
- [GitHub CLI](https://cli.github.com/), signed in: `gh auth status`.

## 1. Prepare the version

On a `tero-k/release-X.Y.Z` branch:

1. Set the same version in `Cargo.toml` (`[workspace.package] version`) and `Packager.toml`
   (`version`). The release workflow fails if they differ from the tag.
2. `cargo check` so `Cargo.lock` picks up the new version.
3. In `CHANGELOG.md`, move the `[Unreleased]` items under a new `## [X.Y.Z] - YYYY-MM-DD`
   heading (highlights first, then known issues) and add its link at the bottom. This section
   becomes the release notes; the release workflow fails without it. Use absolute links there,
   because relative links break on the release page.
4. Run `cargo audit` and update the dependency audit in `SECURITY.md` if anything changed.
5. Update the milestones in `README.md` if needed.
6. Commit, merge into `master`, and wait for CI to pass.

## 2. Tag and push

```sh
git checkout master
git pull
git tag -a vX.Y.Z -m "Juttusihti X.Y.Z"
git push origin vX.Y.Z
```

A tag with a suffix such as `v0.2.0-rc.1` makes a pre-release.

The **Release** workflow runs CI (fmt, clippy, tests, audit), builds the Linux packages on
Ubuntu 22.04, attests them (public repositories only), and creates a draft release named
"Juttusihti X.Y.Z". Its notes are the tag's `CHANGELOG.md` section followed by the download
and verify text in `packaging/release-notes.md`. Re-running the workflow replaces the Linux
files on the draft but keeps its notes; it refuses to touch a release that is already
published.

```sh
gh run watch                       # or follow it under Actions
gh release view vX.Y.Z             # the draft exists and lists the Linux files
```

## 3. Build, sign and upload Windows

From a clean checkout of the tag:

```powershell
git checkout vX.Y.Z
.\packaging\release-windows.ps1 -Tag vX.Y.Z -CertificateThumbprint <SHA-1 thumbprint>
```

With another signing tool, pass a command with `%1` for the file instead (cargo-packager
splits it on spaces, so arguments cannot contain spaces):

```powershell
.\packaging\release-windows.ps1 -Tag vX.Y.Z -SignCommand "C:\tools\sign.cmd %1"
```

The script checks that HEAD is the tag and the versions match, and that the draft exists. It
builds the app and the full-article helper `juttusihti-browser.exe` (from `crates/browser`) and
signs the helper with the same signing option, because cargo-packager signs only the main binary.
It then builds the NSIS installer with cargo-packager, with `beforePackagingCommand` removed from
the config so that no build can replace the signed helper. cargo-packager signs `juttusihti.exe`,
the uninstaller and the installer. The script checks the Authenticode signatures, including the
two exes unpacked from the installer with 7-Zip, and writes these files to `dist\`:

- `juttusihti_X.Y.Z_x64-setup.exe`
- `juttusihti-X.Y.Z-x86_64-windows.zip` (the signed `juttusihti.exe` and `juttusihti-browser.exe` with README and licenses)
- `SHA256SUMS-windows.txt`

Finally it runs `gh release upload vX.Y.Z dist\* --clobber`. Use `-NoUpload` to build and check
without uploading, then upload yourself:

```powershell
gh release upload vX.Y.Z (Get-ChildItem dist -File).FullName --clobber
```

## 4. Review and publish

1. Open the draft: `gh release view vX.Y.Z --web`.
2. Check the files: `.deb`, `.rpm`, `.AppImage`, `-x86_64-linux.tar.gz`, `SHA256SUMS-linux.txt`,
   `-setup.exe`, `-x86_64-windows.zip` and `SHA256SUMS-windows.txt`.
3. Spot-check the checksums and provenance:

   ```sh
   gh release download vX.Y.Z --dir check
   cd check
   sha256sum -c SHA256SUMS-linux.txt SHA256SUMS-windows.txt
   gh attestation verify juttusihti_X.Y.Z_amd64.deb --repo tero-k/juttusihti
   ```

4. Read the notes, edit them on the draft if needed, then press **Publish release**.

## If something goes wrong

- **Wrong commit tagged, nothing published:** delete the draft and the tag, fix, and tag again.

  ```sh
  gh release delete vX.Y.Z --yes
  git push --delete origin vX.Y.Z
  git tag -d vX.Y.Z
  ```

- **Already published:** do not move the tag. Release a new patch version instead.
