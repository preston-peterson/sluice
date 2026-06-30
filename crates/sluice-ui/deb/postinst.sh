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

# Register the service, (re)start it, and VERIFY the new engine actually came up — rolling back to
# the previous engine if it doesn't (task #20). Default-allow means the brief gap can't lock anyone
# out; recovery is always 'sudo systemctl stop sluice-engine'.
if [ -d /run/systemd/system ]; then
  systemctl daemon-reload || true
  systemctl enable sluice-engine.service >/dev/null 2>&1 || true

  READY=/run/sluice/engine.ready
  BK=/var/lib/sluice/engine-rollback
  rm -f "$READY" 2>/dev/null || true
  systemctl restart sluice-engine.service || true

  # Wait up to ~15s for the engine to signal it attached connect4/6 (the eBPF verifier passed).
  ok=0
  i=0
  while [ "$i" -lt 30 ]; do
    if [ -e "$READY" ]; then ok=1; break; fi
    sleep 0.5
    i=$((i + 1))
  done

  if [ "$ok" = 1 ]; then
    echo "sluice: engine is up (connect4/6 attached)."
    rm -rf "$BK" 2>/dev/null || true
  elif [ -f "$BK/sluice-engine" ] && [ -f "$BK/sluice-ebpf" ]; then
    echo "sluice: WARNING — the updated engine did not come up; rolling back to the previous engine." >&2
    cp -f "$BK/sluice-engine" "$LIBDIR/sluice-engine"
    cp -f "$BK/sluice-ebpf"  "$LIBDIR/sluice-ebpf"
    rm -f "$READY" 2>/dev/null || true
    systemctl restart sluice-engine.service || true
    j=0
    while [ "$j" -lt 30 ]; do
      if [ -e "$READY" ]; then break; fi
      sleep 0.5
      j=$((j + 1))
    done
    if [ -e "$READY" ]; then
      echo "sluice: rolled back to the previous engine — it's running. The app updated, but the engine kept the known-good build; please report this update failure." >&2
    elif systemctl is-active --quiet sluice-engine.service; then
      # The restored engine may predate the readiness marker (first upgrade to a marker-aware
      # build); an active service is a good-enough signal that the known-good engine came back.
      echo "sluice: rolled back to the previous engine — it's active (an older build without a readiness marker). The app updated; please report this update failure." >&2
    else
      echo "sluice: rollback engine did not come up either — check 'journalctl -u sluice-engine -n 50'. Recovery: sudo systemctl stop sluice-engine" >&2
    fi
  else
    echo "sluice: WARNING — engine did not come up and no rollback snapshot was found — check 'journalctl -u sluice-engine -n 50'." >&2
  fi
fi

# Make the app-menu entry + icon appear without a re-login (no-op if the tools are absent).
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q /usr/share/applications 2>/dev/null || true
command -v gtk-update-icon-cache  >/dev/null 2>&1 && gtk-update-icon-cache  -q -f /usr/share/icons/hicolor 2>/dev/null || true

exit 0
