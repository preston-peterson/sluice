#!/usr/bin/env bash
# =============================================================================
# Sluice — build a release + (optionally) publish it to GitHub
# =============================================================================
#
# Builds the combined .deb (desktop UI + the prebuilt engine: eBPF object + loader, plus the
# systemd unit and install hooks), writes a SHA-256 checksum, and stages both under
# dist/. The result installs the whole product with no build toolchain on the target.
# With --publish it creates a GitHub Release tagged v<VERSION> (from the
# repo VERSION file) and uploads the artifacts — which is what the in-app
# "Check for updates" reads (the latest release tag).
#
# Usage:
#   ./scripts/package-release.sh            # build + checksum into dist/
#   ./scripts/package-release.sh --publish  # also create/upload the GitHub release
#
# Bump the version first by editing VERSION (and crates/sluice-ui/tauri.conf.json
# so the .deb filename matches), in its own commit.
# =============================================================================
set -euo pipefail

GREEN='\033[32m\033[1m'; CYAN='\033[36m\033[1m'; YELLOW='\033[33m\033[1m'; RESET='\033[0m'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
export PROTOC="${PROTOC:-$(command -v protoc || echo /usr/bin/protoc)}"

PUBLISH=0
[[ "${1:-}" == "--publish" ]] && PUBLISH=1
[[ "${1:-}" == "--help" || "${1:-}" == "-h" ]] && { sed -n '2,18p' "$0" | sed 's/^# \{0,1\}//'; exit 0; }

VERSION="$(tr -d ' \n' < VERSION)"
TAG="v${VERSION}"
echo -e "${CYAN}Packaging Sluice ${TAG}${RESET}"

# 1. Build the engine (eBPF object on nightly + the host loader) and stage the PREBUILT
#    artifacts where the Tauri deb 'files' map can find them. This is what makes the release
#    deb install on a machine with no build toolchain — the engine ships prebuilt inside it.
echo -e "${CYAN}==>${RESET} Building the engine (eBPF object + loader)"
command -v bpf-linker >/dev/null 2>&1 || { echo "bpf-linker required to build the eBPF object: cargo install bpf-linker" >&2; exit 1; }
( cd engine/ebpf && cargo build --release )      # rust-toolchain.toml pins nightly here
( cd engine/loader && cargo build --release )
STAGE="crates/sluice-ui/dist-engine"
mkdir -p "$STAGE"
install -m 0755 engine/loader/target/release/sluice-engine                 "$STAGE/sluice-engine"
install -m 0644 engine/ebpf/target/bpfel-unknown-none/release/sluice-ebpf  "$STAGE/sluice-ebpf"
install -m 0644 engine/sluice-engine.service                               "$STAGE/sluice-engine.service"
echo -e "  ${GREEN}✓${RESET} staged engine artifacts → $STAGE"

# 2. Build the combined .deb (UI + the staged engine, with the install/remove hooks)
echo -e "${CYAN}==>${RESET} Building the combined .deb (UI + engine)"
cargo tauri --version >/dev/null 2>&1 || cargo install tauri-cli --locked
( cd crates/sluice-ui && cargo tauri build --bundles deb )
DEB="$(ls -t target/release/bundle/deb/*.deb 2>/dev/null | head -1 || true)"
[[ -n "$DEB" ]] || { echo "no .deb produced" >&2; exit 1; }

# 3. Stage under dist/ with a checksum
mkdir -p dist
cp -f "$DEB" dist/
DEB_NAME="$(basename "$DEB")"
( cd dist && sha256sum "$DEB_NAME" > "${DEB_NAME}.sha256" )
echo -e "  ${GREEN}✓${RESET} dist/${DEB_NAME}"
echo -e "  ${GREEN}✓${RESET} dist/${DEB_NAME}.sha256"

# 4. Optionally publish a GitHub release
if [[ $PUBLISH -eq 1 ]]; then
  command -v gh >/dev/null 2>&1 || { echo "gh (GitHub CLI) required for --publish" >&2; exit 1; }
  echo -e "${CYAN}==>${RESET} Publishing GitHub release ${TAG}"
  if gh release view "$TAG" >/dev/null 2>&1; then
    echo -e "  ${YELLOW}!${RESET} release ${TAG} already exists — uploading artifacts (clobber)"
    gh release upload "$TAG" "dist/${DEB_NAME}" "dist/${DEB_NAME}.sha256" --clobber
  else
    gh release create "$TAG" "dist/${DEB_NAME}" "dist/${DEB_NAME}.sha256" \
      --title "Sluice ${VERSION}" \
      --notes "See CHANGELOG.md for what's in this release."
  fi
  echo -e "  ${GREEN}✓${RESET} published ${TAG}"
else
  echo -e "\nNot published. Re-run with ${CYAN}--publish${RESET} to create the GitHub release."
fi
