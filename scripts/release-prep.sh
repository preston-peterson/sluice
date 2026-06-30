#!/usr/bin/env bash
# =============================================================================
# Sluice — prepare a release (option C, step 1): bump the version, stamp the
# CHANGELOG, commit, and tag. Then push the tag to trigger the build workflow.
# =============================================================================
# Usage:  scripts/release-prep.sh <version>     # e.g. 0.1.12
#
# Writes the version into VERSION, tauri.conf.json, and the Settings line; moves the CHANGELOG
# [Unreleased] entries into a dated section; commits "release: X.Y.Z"; tags vX.Y.Z. It does NOT
# push — review, then:  git push origin main vX.Y.Z   (that triggers the Release workflow).
# Add your release notes under "## [Unreleased]" in CHANGELOG.md BEFORE running this.
# =============================================================================
set -euo pipefail
GREEN='\033[32m\033[1m'; CYAN='\033[36m\033[1m'; RED='\033[31m\033[1m'; RESET='\033[0m'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
die() { echo -e "${RED}✗${RESET} $*" >&2; exit 1; }

VER="${1:?usage: release-prep.sh <version>  (e.g. 0.1.12)}"; VER="${VER#v}"; TAG="v${VER}"
[[ "$VER" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must be X.Y.Z (got: $VER)"
[[ -z "$(git status --porcelain)" ]] || die "working tree is dirty — commit or stash first."
git rev-parse "$TAG" >/dev/null 2>&1 && die "tag $TAG already exists."

PREV="$(git tag -l 'v*' --sort=-v:refname | head -1 || true)"
DATE="$(date +%Y-%m-%d)"

echo -e "${CYAN}==>${RESET} Bumping version to ${VER}"
echo "$VER" > VERSION
sed -i "s/\"version\": \"[0-9][0-9.]*\"/\"version\": \"$VER\"/" crates/sluice-ui/tauri.conf.json
sed -i "s|id=\"set-version\">Sluice [0-9][0-9.]*<|id=\"set-version\">Sluice $VER<|" crates/sluice-ui/frontend/index.html

echo -e "${CYAN}==>${RESET} Stamping CHANGELOG (${VER} — ${DATE})"
VER="$VER" DATE="$DATE" PREV="$PREV" python3 - <<'PY'
import os, re, sys
ver, date, prev = os.environ["VER"], os.environ["DATE"], os.environ["PREV"]
base = "https://github.com/preston-peterson/sluice"
p = "CHANGELOG.md"; s = open(p).read()
m = re.search(r'## \[Unreleased\]\n(.*?)(?=\n## \[)', s, re.S)
if not m: sys.exit("couldn't find an [Unreleased] section in CHANGELOG.md")
body = m.group(1).strip()
if body in ("", "Nothing yet."):
    sys.exit("CHANGELOG [Unreleased] is empty — add release notes before running release-prep.")
s = s[:m.start()] + "## [Unreleased]\n\nNothing yet.\n\n## [%s] — %s\n\n%s\n" % (ver, date, body) + s[m.end():]
s = re.sub(r'\[Unreleased\]: .*', "[Unreleased]: %s/compare/v%s...HEAD" % (base, ver), s, count=1)
ref = ("[%s]: %s/compare/%s...v%s" % (ver, base, prev, ver)) if prev else ("[%s]: %s/releases/tag/v%s" % (ver, base, ver))
s = re.sub(r'(\[Unreleased\]: .*\n)', r'\1' + ref.replace('\\', r'\\') + "\n", s, count=1)
open(p, "w").write(s)
PY

echo -e "${CYAN}==>${RESET} Committing + tagging ${TAG}"
git add -A
git commit -q -m "release: ${VER}"
git tag "$TAG"

echo -e "\n${GREEN}Prepared ${TAG}.${RESET} Review the diff, then:"
echo -e "  ${CYAN}git push origin main ${TAG}${RESET}    # triggers the build workflow (drafts the release)"
echo -e "  ${CYAN}just sign-release ${VER}${RESET}        # sign locally + publish, once the draft is ready"
