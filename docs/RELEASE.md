# Releasing AudioGraph

This document covers the full path from "version is good" to "signed
installers on a GitHub Release." It's split into three stages:

1. **Cut a release** — bump versions, update CHANGELOG, tag.
2. **Let CI build** — `.github/workflows/release.yml` fires on tag push.
3. **(Optional) sign + notarize** — if the right GitHub secrets are set,
   tauri-action signs everything. Otherwise you ship unsigned artifacts.

## 1. Cut a release

```bash
cd apps/audio-graph

# Bump the three version locations + rotate CHANGELOG.md.
./scripts/bump-version.sh 0.2.0

# Review: should touch package.json + src-tauri/Cargo.toml +
# src-tauri/tauri.conf.json (versions) and CHANGELOG.md (rotation).
git diff

# Commit, tag, push.
git add -A
git commit -m "chore: release 0.2.0"
git tag -a v0.2.0 -m "Release 0.2.0"
git push origin master v0.2.0
```

Tag push → `release.yml` workflow fires automatically.

### Pre-releases

The script accepts any `X.Y.Z[-prerelease]` string:

```bash
./scripts/bump-version.sh 0.2.0-rc.1
git tag v0.2.0-rc.1
```

tauri-action still treats these as draft releases — you have to publish
them by hand from the GitHub Releases page.

## 2. What CI does on tag push

`.github/workflows/release.yml`:

1. **Create draft release** on ubuntu-latest.
2. **Parallel builds** on macOS, Linux (Ubuntu 22.04), Windows.
3. For macOS, builds a **universal binary** (arm64 + x86_64) so one DMG
   runs on both Apple Silicon and Intel Macs.
4. Each platform produces its native installer(s):
   - **macOS:** `.dmg` (disk image) + `.app.tar.gz` (zipped bundle).
   - **Windows:** `.msi` (MSI installer) + `.exe` (NSIS installer).
   - **Linux:** `.AppImage` (portable) + `.deb` (Debian/Ubuntu).
5. All artifacts attach to the draft release created in step 1.
6. Workflow leaves the release **as a draft** — you review the files,
   polish the release notes, and hit "Publish release" by hand.

### Manual dispatch

You can also trigger the workflow from the Actions UI (`workflow_dispatch`).
Useful for testing the build pipeline on branches. Set `dry_run: true` to
skip the GitHub Release publication and just verify artifacts build.

## 3. Code signing + notarization (optional)

Without any signing secrets configured, artifacts still build but:

- **macOS:** users see `"AudioGraph can't be opened because Apple cannot
  check it for malicious software"` on first launch. Right-click → Open
  bypasses, but Gatekeeper will nag. Fatal for any real distribution.
- **Windows:** SmartScreen shows an "Unrecognized app" warning. Users can
  click "More info → Run anyway" but most won't.
- **Linux:** no signing infrastructure to worry about. AppImage + deb
  both ship unsigned by default and nobody notices.

To enable signing, add the corresponding GitHub Actions secrets to the
repository (**Settings → Secrets and variables → Actions**). `release.yml`
already forwards all of these to tauri-action via `env:` — you just need
to populate them.

### Apple (macOS)

You need an **Apple Developer Program membership** ($99/year) and a
**Developer ID Application certificate**.

| Secret | How to get it |
|--------|---------------|
| `APPLE_CERTIFICATE` | Export your Developer ID cert as a `.p12` from Keychain Access, then `base64 -i DeveloperID.p12 \| pbcopy`. Paste the base64 string. |
| `APPLE_CERTIFICATE_PASSWORD` | The password you set when exporting the `.p12`. |
| `APPLE_SIGNING_IDENTITY` | The Common Name (CN) of the cert, e.g. `Developer ID Application: Your Name (TEAMID)`. Get with `security find-identity -v -p codesigning`. |
| `APPLE_ID` | Your Apple ID email. |
| `APPLE_PASSWORD` | An **app-specific password** (not your Apple ID password). Generate at https://appleid.apple.com → "App-Specific Passwords". |
| `APPLE_TEAM_ID` | The 10-character team ID visible at https://developer.apple.com/account → Membership. |

With all six present, tauri-action signs the app bundle AND submits the
DMG for notarization (Apple's out-of-band malware scan) before the
workflow completes. Notarization typically takes 5–15 minutes.

### Windows (Authenticode)

You need an **Authenticode code signing certificate** from a CA like
DigiCert, Sectigo, or SSL.com (~$300–$500/year). EV certs get instant
SmartScreen reputation; OV certs take months of signed-binary telemetry.

| Secret | Notes |
|--------|-------|
| `WINDOWS_CERTIFICATE` | Base64-encoded `.pfx` (PKCS#12). |
| `WINDOWS_CERTIFICATE_PASSWORD` | Password for the `.pfx`. |

tauri-action signs the MSI and NSIS installer if both are present.

### Tauri updater signing (separate)

If you later wire up Tauri's built-in auto-updater, it uses its **own**
signing key (distinct from OS code signing). Generate once:

```bash
bun tauri signer generate -w ~/.tauri/audiograph-updater.key
# Prints a public key — put that into tauri.conf.json → plugins → updater.
# Put the private key contents into the TAURI_SIGNING_PRIVATE_KEY secret.
# Put the password you set into TAURI_SIGNING_PRIVATE_KEY_PASSWORD.
```

Without these, the app bundle still builds but the updater can't verify
update signatures (so leave updater disabled until the keys exist).

## 4. Troubleshooting

**`rsac` path dep not found in CI.** The release workflow stages the
parent rsac repo around the audio-graph checkout (same trick as the PR
CI — see `.github/workflows/ci.yml`). If you see `failed to load source
for dependency rsac` in a build log, something in that staging step is
wrong.

**Notarization hangs.** Apple's notary service has had multi-hour
outages. Check https://www.apple.com/support/systemstatus/. If it's
down, cancel the workflow and re-run when the service is healthy.

**Artifacts are unsigned but secrets look right.** tauri-action is strict
about all-or-nothing: missing any one of the 6 Apple secrets → silently
skips signing. Double-check the `APPLE_SIGNING_IDENTITY` exactly matches
`security find-identity -v -p codesigning` output, case and all.

**Draft release doesn't have all artifacts.** The workflow uses
`fail-fast: false` so a macOS notarization failure doesn't kill the
Linux/Windows builds. Check the failed job's log and re-run just that
matrix entry from the Actions UI.

## 5. Checklist for cutting a release

- [ ] All PRs merged; CI green on master.
- [ ] `./scripts/bump-version.sh X.Y.Z` run and diff reviewed.
- [ ] CHANGELOG entries written under the new version section.
- [ ] `git tag -a vX.Y.Z -m "Release X.Y.Z"`.
- [ ] `git push origin master vX.Y.Z`.
- [ ] Watch the Release workflow finish (typically 20–30 minutes).
- [ ] Review the draft release on GitHub.
- [ ] Download + smoke-test the DMG / MSI / AppImage on real hardware.
- [ ] Publish the release.
