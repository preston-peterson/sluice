#!/usr/bin/env bash
# =============================================================================
# Sluice — sign the LOCALLY-built release .deb with the offline key, then publish.
# =============================================================================
# The .deb was built on this machine (scripts/package-release.sh) and staged in dist/, so you sign
# exactly what you built — nothing built on a remote runner. This is the one step that needs the
# key passphrase (paste works: Ctrl+Shift+V). The key never leaves this machine.
#
# Usage:  scripts/sign-release.sh <version>     # e.g. 0.1.13  (or v0.1.13)
# Needs:  minisign + the secret key (SLUICE_MINISIGN_KEY, default
#         ~/git/sluice-internal/sluice-minisign.key), and gh (authed).
# =============================================================================
set -euo pipefail
GREEN='\033[32m\033[1m'; CYAN='\033[36m\033[1m'; RED='\033[31m\033[1m'; RESET='\033[0m'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
step() { echo -e "\n${CYAN}==>${RESET} $*"; }
die()  { echo -e "${RED}✗${RESET} $*" >&2; exit 1; }

VER="${1:?usage: sign-release.sh <version>  (e.g. 0.1.13)}"; VER="${VER#v}"; TAG="v${VER}"
PUB="$ROOT/crates/sluice-ui/sluice-release.pub"
SIGN_KEY="${SLUICE_MINISIGN_KEY:-$HOME/git/sluice-internal/sluice-minisign.key}"

command -v minisign >/dev/null 2>&1 || die "minisign is required (sudo apt install minisign)."
command -v gh >/dev/null 2>&1       || die "gh (GitHub CLI) is required."
[[ -f "$SIGN_KEY" ]]                || die "signing key not found: $SIGN_KEY (set SLUICE_MINISIGN_KEY)."

DEB="$(ls dist/*_"${VER}"_amd64.deb 2>/dev/null | head -1 || true)"
[[ -n "$DEB" ]] || die "no local build dist/*_${VER}_amd64.deb — build it first: just release"
NAME="$(basename "$DEB")"
gh release view "$TAG" >/dev/null 2>&1 || die "no release $TAG — build the draft first: just release"

step "Verifying the local build's checksum"
( cd dist && sha256sum -c "${NAME}.sha256" )

step "Signing ${NAME} (you'll be prompted for the key passphrase)"
minisign -Sm "$DEB" -s "$SIGN_KEY" -t "Sluice ${VER}"
[[ -f "${DEB}.minisig" ]] || die "signing failed: no .minisig produced."

step "Verifying the signature against the committed public key"
minisign -Vm "$DEB" -p "$PUB" || die "signature did not verify against $PUB — refusing to publish."

step "Uploading the signed local build + publishing ${TAG}"
gh release upload "$TAG" "$DEB" "dist/${NAME}.sha256" "${DEB}.minisig" --clobber
gh release edit "$TAG" --draft=false --latest

echo -e "\n${GREEN}Published ${TAG} (signed, built locally).${RESET} The in-app updater will now offer it."
