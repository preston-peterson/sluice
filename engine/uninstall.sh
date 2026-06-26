#!/usr/bin/env bash
# Remove the Sluice engine systemd service + installed files (E3.2). Run as root.
# Leaves /var/lib/sluice/rules.json in place (your rules); pass --purge to remove it too.
set -euo pipefail
[[ $EUID -eq 0 ]] || { echo "Run as root: sudo $0" >&2; exit 1; }

systemctl disable --now sluice-engine 2>/dev/null || true
rm -f /etc/systemd/system/sluice-engine.service
systemctl daemon-reload
rm -rf /usr/lib/sluice
if [[ "${1:-}" == "--purge" ]]; then
  rm -rf /var/lib/sluice
  echo "[uninstall] removed engine + rules store."
else
  echo "[uninstall] removed engine (kept /var/lib/sluice/rules.json; --purge to remove)."
fi
