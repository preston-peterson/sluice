#!/usr/bin/env bash
# =============================================================================
# Sluice — sign a CI-built DRAFT release locally, then publish it (option C).
# =============================================================================
# The Release workflow builds the .deb in CI and leaves a DRAFT release. This script (run on the
# maintainer's machine, where the OFFLINE signing key lives) downloads that artifact, signs it,
# verifies the signature against the committed public key, uploads the .minisig, and publishes.
#
# The signing key never leaves your machine — that's the whole point of the hybrid flow.
#
# Usage:  scripts/sign-release.sh <version>     # e.g. 0.1.12  (or v0.1.12)
# Needs:  gh (authed), minisign, and the secret key (SLUICE_MINISIGN_KEY, default
#         ~/git/sluice-internal/sluice-minisign.key).
# =============================================================================
set -euo pipefail
GREEN='\033[32m\033[1m'; CYAN='\033[36m\033[1m'; RED='\033[31m\033[1m'; RESET='\033[0m'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
step() { echo -e "\n${CYAN}==>${RESET} $*"; }
die()  { echo -e "${RED}✗${RESET} $*" >&2; exit 1; }

VER="${1:?usage: sign-release.sh <version>  (e.g. 0.1.12)}"; VER="${VER#v}"; TAG="v${VER}"
PUB="$ROOT/crates/sluice-ui/sluice-release.pub"
SIGN_KEY="${SLUICE_MINISIGN_KEY:-$HOME/git/sluice-internal/sluice-minisign.key}"

command -v gh >/dev/null 2>&1       || die "gh (GitHub CLI) is required."
command -v minisign >/dev/null 2>&1 || die "minisign is required (sudo apt install minisign)."
[[ -f "$SIGN_KEY" ]]                || die "signing key not found: $SIGN_KEY (set SLUICE_MINISIGN_KEY)."
gh release view "$TAG" >/dev/null 2>&1 || die "no release $TAG found — push the tag first so CI drafts it."

WORK="$(mktemp -d)"; trap 'rm -rf "$WORK"' EXIT

step "Downloading the draft ${TAG} artifacts"
gh release download "$TAG" --pattern '*.deb' --pattern '*.deb.sha256' --dir "$WORK"
DEB="$(ls "$WORK"/*.deb 2>/dev/null | head -1 || true)"
[[ -n "$DEB" ]] || die "no .deb in release $TAG (did the build workflow finish?)."
NAME="$(basename "$DEB")"

step "Verifying the CI checksum"
( cd "$WORK" && sha256sum -c "${NAME}.sha256" )

step "Signing ${NAME} (you'll be prompted for the key passphrase)"
minisign -Sm "$DEB" -s "$SIGN_KEY" -t "Sluice ${VER}"
[[ -f "${DEB}.minisig" ]] || die "signing failed: no .minisig produced."

step "Verifying the signature against the committed public key"
minisign -Vm "$DEB" -p "$PUB" || die "signature did not verify against $PUB — refusing to publish."

step "Uploading the signature + publishing ${TAG}"
gh release upload "$TAG" "${DEB}.minisig" --clobber
gh release edit "$TAG" --draft=false --latest

echo -e "\n${GREEN}Published ${TAG} (signed).${RESET} The in-app updater will now offer it."
