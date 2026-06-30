#!/usr/bin/env bash
# Sluice dev bootstrap — idempotent. Installs the build toolchain (protoc + Rust) and
# verifies prerequisites. Safe to run repeatedly.
#
# What it does NOT do: install the Sluice engine. That is a root systemd service
# (`sudo engine/install.sh`); this script only *reports* whether it's present.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
info() { printf '  %s\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
warn() { printf '  \033[33m!\033[0m %s\n' "$*"; }

have() { command -v "$1" >/dev/null 2>&1; }

# Pick a privilege escalator only if we actually need apt and aren't already root.
SUDO=""
if [ "$(id -u)" -ne 0 ]; then
  if have sudo; then SUDO="sudo"; fi
fi

bold "Sluice setup — toolchain & prerequisites"

# 1. System build deps (protoc, pkg-config, a C toolchain for some -sys crates).
bold "1. System packages (protoc, pkg-config, build-essential)"
if have protoc; then
  ok "protoc present: $(protoc --version)"
else
  if have apt-get; then
    info "installing protobuf-compiler pkg-config build-essential via apt…"
    $SUDO apt-get update -y
    $SUDO apt-get install -y protobuf-compiler pkg-config build-essential
    ok "protoc installed: $(protoc --version)"
  else
    warn "apt-get not found. Install a protobuf compiler manually so 'protoc' is on PATH."
    warn "  (protoc is required for gRPC codegen.)"
  fi
fi

# 2. Rust toolchain via rustup. rustup reads rust-toolchain.toml and installs the pinned
#    channel automatically on first cargo invocation inside the repo.
bold "2. Rust toolchain (rustup, honoring rust-toolchain.toml)"
if have cargo || [ -x "$HOME/.cargo/bin/cargo" ]; then
  ok "cargo present"
else
  info "installing rustup (non-interactive)…"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
  warn "Run 'source \"\$HOME/.cargo/env\"' or open a new shell so cargo is on PATH."
fi
# Make cargo usable within this script run if it was just installed.
if ! have cargo && [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
if have cargo; then
  info "provisioning the pinned toolchain (this may download it once)…"
  cargo --version || true
  ok "cargo: $(cargo --version 2>/dev/null || echo 'on PATH after you re-source ~/.cargo/env')"
fi

# 3. Tauri UI system libraries (WebKitGTK et al). Needed to build/run crates/sluice-ui.
bold "3. Tauri UI system libraries (WebKitGTK, libsoup, …)"
TAURI_LIBS="libwebkit2gtk-4.1-dev libsoup-3.0-dev librsvg2-dev libxdo-dev libayatana-appindicator3-dev libssl-dev"
if pkg-config --exists webkit2gtk-4.1 2>/dev/null; then
  ok "WebKitGTK 4.1 present ($(pkg-config --modversion webkit2gtk-4.1))."
elif have apt-get; then
  info "installing Tauri deps: $TAURI_LIBS"
  $SUDO apt-get install -y $TAURI_LIBS
  ok "Tauri UI libraries installed."
else
  warn "apt-get not found. Install the Tauri prerequisites for your distro to build crates/sluice-ui:"
  warn "  https://tauri.app/start/prerequisites/"
fi

# 3b. Tauri CLI — needed only for `just package` (builds the installable .deb). Idempotent.
bold "3b. Tauri CLI (for 'just package' .deb bundling)"
if ! have cargo; then
  warn "cargo not on PATH yet; re-run setup after 'source \$HOME/.cargo/env' to install the Tauri CLI."
elif cargo tauri --version >/dev/null 2>&1; then
  ok "tauri-cli present: $(cargo tauri --version 2>/dev/null | head -1)"
else
  info "installing tauri-cli (cargo install tauri-cli) — a few minutes; only needed for packaging…"
  cargo install tauri-cli --locked || warn "tauri-cli install failed; run 'cargo install tauri-cli' before 'just package'."
fi

# 3c. just — the task runner the repo's docs/justfile use ('just check', 'just release-prep', …).
bold "3c. just (task runner)"
if have just; then
  ok "just present: $(just --version 2>/dev/null)"
elif ! have cargo; then
  warn "cargo not on PATH yet; re-run setup after 'source \$HOME/.cargo/env' to install just."
else
  info "installing just (cargo install just)…"
  cargo install just --locked || warn "just install failed; the underlying scripts still work standalone (see the justfile)."
fi

# 4. Sluice engine status — report only (never install/modify the firewall here).
bold "4. Sluice engine status (report only)"
if have systemctl && systemctl list-unit-files 2>/dev/null | grep -q '^sluice-engine'; then
  ok "sluice-engine service is installed."
  state="$(systemctl is-active sluice-engine 2>/dev/null || echo unknown)"
  info "service state: $state"
else
  warn "sluice-engine not installed. Build + install it (needs sudo):"
  warn "  just engine-build && sudo engine/install.sh"
fi

# 5. Git hooks — wire the tracked pre-push guard (blocks leaking internal files / forbidden strings).
bold "5. Git hooks (pre-push guard)"
if [ -d .githooks ]; then
  chmod +x .githooks/* 2>/dev/null || true
  git config core.hooksPath .githooks
  ok "pre-push guard wired (core.hooksPath=.githooks)"
else
  warn ".githooks/ not found; pre-push guard not wired."
fi

bold "Done."
info "Next: 'just check' (fmt + clippy + tests), then 'just engine-build'."
