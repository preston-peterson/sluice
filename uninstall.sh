#!/usr/bin/env bash
# =============================================================================
# Sluice — uninstaller
# =============================================================================
#
# Reverses install.sh: removes the engine service + the desktop UI .deb. Your
# data (engine rule store + UI history) is kept unless you ask otherwise.
#
# Usage:
#   ./uninstall.sh           # remove engine + UI; prompt about your data
#   ./uninstall.sh --purge   # remove everything, data included, no prompts
#   ./uninstall.sh --keep    # remove engine + UI, keep all data, no prompts
#   ./uninstall.sh --help
#
# Removed always:  the sluice-engine service + /usr/lib/sluice, the sluice-ui package.
# Your data:       /var/lib/sluice (rules) and ~/.local/share/sluice (history).
# Stopping the engine reopens inbound traffic automatically.
# =============================================================================
set -euo pipefail

GREEN='\033[32m\033[1m'; YELLOW='\033[33m\033[1m'; CYAN='\033[36m\033[1m'; RED='\033[31m\033[1m'; RESET='\033[0m'
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ok() { echo -e "  ${GREEN}✓${RESET} $*"; }
step() { echo -e "\n${CYAN}==>${RESET} $*"; }
have() { command -v "$1" >/dev/null 2>&1; }

MODE="prompt"
case "${1:-}" in
  --purge) MODE="purge" ;;
  --keep)  MODE="keep" ;;
  --help|-h) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
  "") ;;
  *) echo "unknown option: ${1} (try --help)" >&2; exit 1 ;;
esac

[[ $EUID -eq 0 ]] && { echo "Run as your normal user (it sudo's where needed)." >&2; exit 1; }
have sudo || { echo "sudo is required." >&2; exit 1; }

echo -e "${CYAN}Sluice uninstaller${RESET}"

# 1. Engine service + installed files (reopens inbound on stop). Keeps /var/lib/sluice for now.
step "Removing the engine service"
if [[ -x "$ROOT/engine/uninstall.sh" ]]; then
  sudo "$ROOT/engine/uninstall.sh" || true
else
  sudo systemctl disable --now sluice-engine 2>/dev/null || true
  sudo rm -f /etc/systemd/system/sluice-engine.service
  sudo systemctl daemon-reload 2>/dev/null || true
  sudo rm -rf /usr/lib/sluice
fi
ok "engine service removed"

# 2. Desktop UI package
step "Removing the desktop UI"
if dpkg -s sluice-ui >/dev/null 2>&1; then
  sudo apt-get remove -y sluice-ui 2>/dev/null || sudo dpkg -r sluice-ui || true
  ok "sluice-ui package removed"
else
  echo "  (sluice-ui package not installed — skipping)"
fi

# 3. Data — rules store (/var/lib/sluice) + UI history (~/.local/share/sluice)
ENGINE_DATA="/var/lib/sluice"
UI_DATA="${XDG_DATA_HOME:-$HOME/.local/share}/sluice"
remove_data() {
  sudo rm -rf "$ENGINE_DATA"
  rm -rf "$UI_DATA"
  ok "removed rule store + history"
}
case "$MODE" in
  purge) step "Removing data (--purge)"; remove_data ;;
  keep)  step "Keeping data (--keep)"; echo "  kept $ENGINE_DATA and $UI_DATA" ;;
  prompt)
    step "Your data"
    echo "  Rule store:  $ENGINE_DATA"
    echo "  UI history:  $UI_DATA"
    read -r -p "  Delete these too? [y/N] " ans
    if [[ "${ans,,}" == "y" || "${ans,,}" == "yes" ]]; then remove_data; else echo "  kept your data"; fi
    ;;
esac

echo -e "\n${GREEN}Sluice uninstalled.${RESET} Inbound traffic is open again (engine stopped)."
