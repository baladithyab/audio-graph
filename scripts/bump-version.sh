#!/usr/bin/env bash
#
# Bump the app version across the three places Tauri expects it to match:
#   - package.json (frontend)
#   - src-tauri/Cargo.toml (backend crate version)
#   - src-tauri/tauri.conf.json (bundle version, shown to the OS)
#
# Also rotates CHANGELOG.md: the current "## [Unreleased]" section becomes
# "## [X.Y.Z] - YYYY-MM-DD" and a fresh Unreleased section is added.
#
# Intentionally dumb — no semver magic, no git automation, no tagging.
# Caller is expected to review the diff, commit, and tag by hand:
#
#   ./scripts/bump-version.sh 0.2.0
#   git diff  # sanity-check all 3 files bumped + CHANGELOG rotated
#   git add -A && git commit -m "chore: release 0.2.0"
#   git tag -a v0.2.0 -m "Release 0.2.0"
#   git push origin master v0.2.0   # tag push fires the Release workflow

set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <new-version>" >&2
    echo "example: $0 0.2.0" >&2
    exit 1
fi

NEW_VERSION="$1"

# Shape check — same regex Cargo applies (major.minor.patch with optional pre-release).
if ! [[ "$NEW_VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "error: version '$NEW_VERSION' doesn't match X.Y.Z[-prerelease]" >&2
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

PACKAGE_JSON="package.json"
CARGO_TOML="src-tauri/Cargo.toml"
TAURI_CONF="src-tauri/tauri.conf.json"
CHANGELOG="CHANGELOG.md"

# ── 1. package.json ────────────────────────────────────────────────────
#
# Intentionally using sed on a JSON file instead of jq: keeps script
# dependency-free (bash + sed are present everywhere). The regex targets
# the canonical top-level "version" key written by npm/bun — if you've
# hand-edited to move "version" into nested objects this will need jq.
if ! grep -qE '^[[:space:]]*"version":[[:space:]]*"[^"]+",?[[:space:]]*$' "$PACKAGE_JSON"; then
    echo "error: couldn't find top-level \"version\" key in $PACKAGE_JSON" >&2
    exit 1
fi
sed -i.bak -E 's/^([[:space:]]*"version":[[:space:]]*")[^"]+(",?[[:space:]]*)$/\1'"$NEW_VERSION"'\2/' "$PACKAGE_JSON"
rm "$PACKAGE_JSON.bak"

# ── 2. src-tauri/Cargo.toml ───────────────────────────────────────────
#
# Only bump the top `[package]` version, not any transitive `version = "..."`
# in dependency entries. We anchor on the first match after `[package]`.
# `awk` is clearest here since sed doesn't natively understand "only the
# first match after X".
awk -v new="$NEW_VERSION" '
    BEGIN { in_pkg = 0; done = 0 }
    /^\[package\]/ { in_pkg = 1; print; next }
    /^\[/ && !/^\[package\]/ { in_pkg = 0 }
    in_pkg && !done && /^version[[:space:]]*=/ {
        print "version = \"" new "\""
        done = 1
        next
    }
    { print }
' "$CARGO_TOML" > "$CARGO_TOML.tmp"
mv "$CARGO_TOML.tmp" "$CARGO_TOML"

# ── 3. src-tauri/tauri.conf.json ──────────────────────────────────────
sed -i.bak -E 's/^([[:space:]]*"version":[[:space:]]*")[^"]+(",?[[:space:]]*)$/\1'"$NEW_VERSION"'\2/' "$TAURI_CONF"
rm "$TAURI_CONF.bak"

# ── 4. CHANGELOG.md rotation ─────────────────────────────────────────
#
# Create if missing with a keepachangelog skeleton.
if [ ! -f "$CHANGELOG" ]; then
    cat > "$CHANGELOG" <<EOF
# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

### Changed

### Fixed

EOF
fi

# Rotate: replace "## [Unreleased]" with "## [Unreleased]\n\n## [X.Y.Z] - DATE"
# and move any bullets that were in Unreleased into the new dated section.
TODAY=$(date -u +%Y-%m-%d)
python3 - <<PY "$CHANGELOG" "$NEW_VERSION" "$TODAY"
import re
import sys

path, new_version, today = sys.argv[1], sys.argv[2], sys.argv[3]
with open(path) as f:
    content = f.read()

# Find "## [Unreleased]" and the next "## [" section header (or EOF).
# Everything between belongs to Unreleased and rolls into the new version.
pattern = re.compile(r"(## \[Unreleased\]\s*\n)(.*?)(?=\n## \[|\Z)", re.DOTALL)
m = pattern.search(content)
if not m:
    print("warning: couldn't find ## [Unreleased] section; appending new entry",
          file=sys.stderr)
    new_entry = f"\n## [{new_version}] - {today}\n\n### Added\n\n"
    content = content + new_entry
else:
    unreleased_body = m.group(2).rstrip() + "\n"
    # If Unreleased was empty (just the Added/Changed/Fixed scaffolding),
    # preserve those subsection headers under the dated version too; users
    # can fill them in post-bump if they forgot beforehand.
    if unreleased_body.strip() == "" or unreleased_body.strip() in {
        "### Added", "### Changed", "### Fixed",
        "### Added\n\n### Changed\n\n### Fixed",
    }:
        unreleased_body = "### Added\n\n### Changed\n\n### Fixed\n"
    replacement = (
        "## [Unreleased]\n\n"
        "### Added\n\n"
        "### Changed\n\n"
        "### Fixed\n\n"
        f"## [{new_version}] - {today}\n\n"
        f"{unreleased_body}"
    )
    content = content[:m.start()] + replacement + content[m.end():]

with open(path, "w") as f:
    f.write(content)
PY

echo "Bumped to $NEW_VERSION in:"
echo "  - $PACKAGE_JSON"
echo "  - $CARGO_TOML"
echo "  - $TAURI_CONF"
echo "  - $CHANGELOG (Unreleased → [$NEW_VERSION] - $TODAY)"
echo
echo "Review the diff:  git diff"
echo "Then commit:      git add -A && git commit -m \"chore: release $NEW_VERSION\""
echo "And tag:          git tag -a v$NEW_VERSION -m \"Release $NEW_VERSION\""
echo "Push:             git push origin master v$NEW_VERSION"
