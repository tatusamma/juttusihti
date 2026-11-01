
## Download

| System | File |
| --- | --- |
| Windows 10/11 (x64) | `juttusihti_X.Y.Z_x64-setup.exe`: per-user installer, no administrator rights needed |
| Windows, portable | `juttusihti-X.Y.Z-x86_64-windows.zip`: unzip and run `juttusihti.exe` |
| Debian, Ubuntu 22.04+ | `juttusihti_X.Y.Z_amd64.deb`: `sudo apt install ./juttusihti_X.Y.Z_amd64.deb` |
| Fedora, openSUSE, RHEL 10 | `juttusihti-RPMVER-1.x86_64.rpm`: `sudo dnf install ./juttusihti-RPMVER-1.x86_64.rpm` (openSUSE: `sudo zypper install`) |
| Other Linux (x86_64) | `juttusihti_X.Y.Z_x86_64.AppImage`: `chmod +x` it and run it |
| Linux, plain binary | `juttusihti-X.Y.Z-x86_64-linux.tar.gz` |

Linux packages need glibc 2.35 or newer; on RHEL 9 and its rebuilds use the AppImage.

## Verify

The Windows files are code-signed. Checksums are in `SHA256SUMS-linux.txt` and
`SHA256SUMS-windows.txt`, and the Linux packages carry build provenance attestations:

```sh
sha256sum -c SHA256SUMS-linux.txt --ignore-missing
gh attestation verify juttusihti_X.Y.Z_amd64.deb --repo tero-k/juttusihti
```

Full changelog: [CHANGELOG.md](https://github.com/tero-k/juttusihti/blob/master/CHANGELOG.md)
