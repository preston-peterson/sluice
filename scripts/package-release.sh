#!/usr/bin/env bash
# =============================================================================
# Sluice — build the release .deb LOCALLY, then create a DRAFT GitHub release.
# =============================================================================
# The package is built ONLY on this machine — never on a remote/CI runner — so what you sign is
# exactly what you built. This step needs no passphrase; run it, then sign with sign-release.sh.
#
# Usage:
#   scripts/package-release.sh          # build -> dist/ + create/refresh the DRAFT release
#   scripts/package-release.sh --no-gh  # just build into dist/ (no GitHub release)
#
# Release flow:
#   just release-prep X.Y.Z          # bump VERSION/tauri.conf/CHANGELOG + commit + tag
#   git push origin main vX.Y.Z      # push the code + tag
#   just release                     # THIS: build the .deb locally + create the draft
#   just sign-release X.Y.Z          # sign the local build (passphrase) + publish
# =============================================================================
set -euo pipefail
GREEN='\033[32m\033[1m'; CYAN='\033[36m\033[1m'; YELLOW='\033[33m\033[1m'; RESET='\033[0m'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
export PROTOC="${PROTOC:-$(command -v protoc || echo /usr/bin/protoc)}"

NO_GH=0
[[ "${1:-}" == "--no-gh" ]] && NO_GH=1
[[ "${1:-}" == "--help" || "${1:-}" == "-h" ]] && { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0; }

VERSION="$(tr -d ' \n' < VERSION)"; TAG="v${VERSION}"
echo -e "${CYAN}Building Sluice ${TAG} locally${RESET}"

# 1. Engine (eBPF object on nightly + the host loader), staged where the .deb bundles it.
echo -e "${CYAN}==>${RESET} Building the engine (eBPF object + loader)"
command -v bpf-linker >/dev/null 2>&1 || { echo "bpf-linker required to build the eBPF object: cargo install bpf-linker" >&2; exit 1; }
( cd engine/ebpf && cargo build --release )      # rust-toolchain.toml pins nightly here
( cd engine/loader && cargo build --release )
STAGE="crates/sluice-ui/dist-engine"; mkdir -p "$STAGE"
install -m 0755 engine/loader/target/release/sluice-engine                 "$STAGE/sluice-engine"
install -m 0644 engine/ebpf/target/bpfel-unknown-none/release/sluice-ebpf  "$STAGE/sluice-ebpf"
install -m 0644 engine/sluice-engine.service                               "$STAGE/sluice-engine.service"
echo -e "  ${GREEN}✓${RESET} staged engine artifacts"

# 2. Combined .deb (UI + the staged engine + install/remove hooks).
echo -e "${CYAN}==>${RESET} Building the combined .deb (UI + engine)"
cargo tauri --version >/dev/null 2>&1 || cargo install tauri-cli --locked
( cd crates/sluice-ui && cargo tauri build --bundles deb )
DEB="$(ls -t target/release/bundle/deb/*.deb 2>/dev/null | head -1 || true)"
[[ -n "$DEB" ]] || { echo "no .deb produced" >&2; exit 1; }

# 3. Stage under dist/ with a checksum.
mkdir -p dist; cp -f "$DEB" dist/; DEB_NAME="$(basename "$DEB")"
( cd dist && sha256sum "$DEB_NAME" > "${DEB_NAME}.sha256" )
echo -e "  ${GREEN}✓${RESET} dist/${DEB_NAME}"
echo -e "  ${GREEN}✓${RESET} dist/${DEB_NAME}.sha256"

# 4. Create/refresh a DRAFT release with the .deb + .sha256 (UNSIGNED). Never published here — a
#    release goes live only when sign-release.sh signs the local build and flips the draft.
if [[ $NO_GH -eq 0 ]]; then
  command -v gh >/dev/null 2>&1 || { echo "gh (GitHub CLI) required (or run with --no-gh)" >&2; exit 1; }
  echo -e "${CYAN}==>${RESET} Creating the DRAFT release ${TAG}"
  if gh release view "$TAG" >/dev/null 2>&1; then
    gh release upload "$TAG" "dist/${DEB_NAME}" "dist/${DEB_NAME}.sha256" --clobber
  else
    gh release create "$TAG" "dist/${DEB_NAME}" "dist/${DEB_NAME}.sha256" \
      --draft --title "Sluice ${VERSION}" \
      --notes "See CHANGELOG.md. DRAFT until signed + published locally (scripts/sign-release.sh)."
  fi
  echo -e "  ${GREEN}✓${RESET} draft ${TAG} ready"
  echo -e "\nNext (the one interactive step): ${CYAN}just sign-release ${VERSION}${RESET}"
else
  echo -e "\nBuilt into dist/ only (--no-gh)."
fi
