#!/bin/sh
# Sluice .deb post-install — register + start the root engine service and resolve the uid
# that's allowed to drive it over its UDS. Runs as root via dpkg/apt. POSIX sh (no bashisms).
set -e

LIBDIR=/usr/lib/sluice
ENVDIR=/etc/sluice
ENVFILE="$ENVDIR/engine.env"

# The files-map copy preserves modes, but make sure the engine bits are right.
chmod 0755 "$LIBDIR/sluice-engine" 2>/dev/null || true
chmod 0644 "$LIBDIR/sluice-ebpf"  2>/dev/null || true

# Resolve the owner uid once (kept across upgrades). Prefer the user behind sudo/pkexec; fall
# back to the first regular login user (the common single-desktop case). If none is found we
# leave the file empty — the UI link stays disabled until the admin sets it (see the note).
if ! grep -qs '^SLUICE_OWNER_UID=[0-9]' "$ENVFILE" 2>/dev/null; then
  owner="${PKEXEC_UID:-${SUDO_UID:-}}"
  if [ -z "$owner" ]; then
    owner=$(getent passwd 2>/dev/null | awk -F: '$3>=1000 && $3<60000 && $7 !~ /(nologin|false)$/ {print $3; exit}')
  fi
  mkdir -p "$ENVDIR"
  if [ -n "$owner" ]; then
    printf 'SLUICE_OWNER_UID=%s\n' "$owner" > "$ENVFILE"
    echo "sluice: engine owner uid set to $owner (edit $ENVFILE then 'systemctl restart sluice-engine' to change)."
  else
    : > "$ENVFILE"
    echo "sluice: could not auto-detect a desktop user — set SLUICE_OWNER_UID=<uid> in $ENVFILE then 'systemctl restart sluice-engine'."
  fi
  chmod 0644 "$ENVFILE"
fi

# Register + (re)start the service (skip in chroot/container builds with no systemd).
if [ -d /run/systemd/system ]; then
  systemctl daemon-reload || true
  systemctl enable sluice-engine.service >/dev/null 2>&1 || true
  # restart (not just start) so an upgrade picks up the new engine binary. Default-allow means
  # the brief gap can't lock anyone out; recovery is always 'sudo systemctl stop sluice-engine'.
  systemctl restart sluice-engine.service || \
    echo "sluice: engine did not start — check 'journalctl -u sluice-engine -n 30'."
fi

# Make the app-menu entry + icon appear without a re-login (no-op if the tools are absent).
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications 2>/dev/null || true
command -v gtk-update-icon-cache  >/dev/null 2>&1 && gtk-update-icon-cache  -q -f /usr/share/icons/hicolor 2>/dev/null || true

exit 0
