#!/bin/sh
# Sluice .deb post-remove — reload systemd after dpkg drops the unit; on purge, remove the
# installer-written config. The rule store (/var/lib/sluice) is intentionally KEPT so an
# accidental purge doesn't wipe your firewall rules; remove it by hand if you really want to. POSIX sh.
set -e

if [ -d /run/systemd/system ]; then
  systemctl daemon-reload >/dev/null 2>&1 || true
fi

if [ "$1" = purge ]; then
  rm -f /etc/sluice/engine.env
  rmdir /etc/sluice 2>/dev/null || true
  echo "sluice: purged. Kept /var/lib/sluice (your rules) — 'sudo rm -rf /var/lib/sluice' to remove."
fi

exit 0
