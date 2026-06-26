#!/usr/bin/env bash
# =============================================================================
# Sluice — one-command installer
# =============================================================================
#
# Builds and installs both halves of Sluice:
#   1. the root engine  → /usr/lib/sluice + a systemd service (sluice-engine)
#   2. the desktop UI    → a .deb installed system-wide (sluice-ui + app menu entry)
#
# Usage:
#   ./install.sh              # full install (builds from source; uses sudo where needed)
#   ./install.sh --skip-deps  # skip the prerequisite step (toolchain already set up)
#   ./install.sh --engine     # install/refresh ONLY the engine
#   ./install.sh --ui         # build/install ONLY the desktop UI (.deb)
#   ./install.sh --help
#
# Run it as your normal user — it calls sudo only for the steps that need root
# (apt, the systemd service, installing the .deb). Recovery is always:
#   sudo systemctl stop sluice-engine
# =============================================================================
set -euo pipefail

GREEN='\033[32m\033[1m'; RED='\033[31m\033[1m'; CYAN='\033[36m\033[1m'; YELLOW='\033[33m\033[1m'; DIM='\033[2m'; RESET='\033[0m'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"

step() { echo -e "\n${CYAN}==>${RESET} $*"; }
ok()   { echo -e "  ${GREEN}✓${RESET} $*"; }
warn() { echo -e "  ${YELLOW}!${RESET} $*"; }
die()  { echo -e "  ${RED}✗${RESET} $*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

DO_DEPS=1; DO_ENGINE=1; DO_UI=1
for a in "$@"; do case "$a" in
  --skip-deps) DO_DEPS=0 ;;
  --engine)    DO_UI=0 ;;
  --ui)        DO_ENGINE=0; DO_DEPS=0 ;;
  --help|-h)   sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  *) die "unknown option: $a (try --help)" ;;
esac; done

[[ $EUID -eq 0 ]] && die "Run as your normal user, not root — the script sudo's only where needed."
have sudo || die "sudo is required."

echo -e "${CYAN}Sluice installer${RESET} ${DIM}($ROOT)${RESET}"

# ---------------------------------------------------------------------------
# 1. Prerequisites — build toolchain (stable + nightly eBPF), protoc, Tauri libs
# ---------------------------------------------------------------------------
if [[ $DO_DEPS -eq 1 ]]; then
  step "Installing prerequisites"
  if [[ -x scripts/setup.sh ]]; then
    ./scripts/setup.sh || warn "setup.sh reported issues; continuing"
  else
    warn "scripts/setup.sh not found; assuming Rust + protoc + WebKitGTK libs are present"
  fi
  have rustup || die "rustup not found — install Rust from https://rustup.rs then re-run."
  # eBPF needs nightly + rust-src + bpf-linker.
  step "Ensuring the eBPF toolchain (nightly + rust-src + bpf-linker)"
  rustup toolchain list | grep -q '^nightly' || rustup toolchain install nightly
  rustup component add rust-src --toolchain nightly >/dev/null 2>&1 || true
  ok "nightly + rust-src ready"
  if ! have bpf-linker; then
    echo "  installing bpf-linker (cargo install bpf-linker — a few minutes)…"
    cargo install bpf-linker || die "bpf-linker install failed"
  fi
  ok "bpf-linker ready"
else
  step "Skipping prerequisites (--skip-deps)"
fi

export PROTOC="${PROTOC:-$(command -v protoc || echo /usr/bin/protoc)}"

# ---------------------------------------------------------------------------
# 2 + 3. Engine — build the eBPF object + loader, install the systemd service
# ---------------------------------------------------------------------------
if [[ $DO_ENGINE -eq 1 ]]; then
  step "Building the engine (eBPF object + root loader)"
  ( cd engine/ebpf && cargo build --release )      # rust-toolchain.toml pins nightly here
  ( cd engine/loader && cargo build --release )
  ok "engine built"

  step "Installing the engine service (needs root)"
  sudo SLUICE_OWNER_UID="${SLUICE_OWNER_UID:-$(id -u)}" SUDO_UID="$(id -u)" ./engine/install.sh
  sudo systemctl enable --now sluice-engine
  if systemctl is-active --quiet sluice-engine; then ok "sluice-engine is running"; else warn "sluice-engine not active — check: journalctl -u sluice-engine -n 30"; fi
fi

# ---------------------------------------------------------------------------
# 4. Desktop UI — build a .deb and install it (app menu entry + sluice-ui binary)
# ---------------------------------------------------------------------------
if [[ $DO_UI -eq 1 ]]; then
  step "Building the desktop UI (.deb)"
  cargo tauri --version >/dev/null 2>&1 || cargo install tauri-cli --locked || warn "tauri-cli install failed; trying anyway"
  ( cd crates/sluice-ui && cargo tauri build --bundles deb )
  DEB="$(ls -t target/release/bundle/deb/*.deb 2>/dev/null | head -1 || true)"
  [[ -n "$DEB" ]] || die "no .deb produced under target/release/bundle/deb/"
  ok "built $(basename "$DEB")"
  step "Installing the UI .deb (needs root)"
  sudo apt-get install -y "$DEB" 2>/dev/null || { sudo dpkg -i "$DEB" || true; sudo apt-get -f install -y; }
  ok "sluice-ui installed"
fi

# ---------------------------------------------------------------------------
echo -e "\n${GREEN}Sluice installed.${RESET}"
echo -e "  • Launch the UI from your app menu (\"Sluice\") or run: ${CYAN}sluice-ui${RESET}"
echo -e "  • Engine service: ${CYAN}systemctl status sluice-engine${RESET}"
echo -e "  • Recovery (open the network back up): ${CYAN}sudo systemctl stop sluice-engine${RESET}"
echo -e "  • Uninstall: ${CYAN}./uninstall.sh${RESET}"
