#!/bin/sh
# Sluice .deb pre-remove — stop + disable the engine on package removal (NOT on upgrade; the
# new postinst restarts it). Stopping tears down the nftables table, reopening inbound. POSIX sh.
set -e

if [ "$1" = remove ] || [ "$1" = deconfigure ] || [ "$1" = purge ]; then
  if [ -d /run/systemd/system ]; then
    systemctl disable --now sluice-engine.service >/dev/null 2>&1 || true
  fi
fi

exit 0
