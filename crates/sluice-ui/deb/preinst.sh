#!/bin/sh
# Sluice .deb pre-install — runs BEFORE the new files are unpacked. On an upgrade, snapshot the
# current (known-good) engine binary + eBPF object so postinst can roll back to them if the new
# engine fails to come up. POSIX sh. Args: "install" | "upgrade <old-version>".
set -e

if [ "$1" = upgrade ]; then
  BK=/var/lib/sluice/engine-rollback
  if [ -f /usr/lib/sluice/sluice-engine ] && [ -f /usr/lib/sluice/sluice-ebpf ]; then
    mkdir -p "$BK"
    cp -f /usr/lib/sluice/sluice-engine "$BK/sluice-engine"
    cp -f /usr/lib/sluice/sluice-ebpf  "$BK/sluice-ebpf"
  fi
fi

exit 0
