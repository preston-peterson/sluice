#!/usr/bin/env bash
# Install the Sluice engine as a systemd service (E3.2 cutover). Run as root AFTER building:
#   ( cd engine/ebpf && cargo build --release ) && ( cd engine/loader && cargo build --release )
#   sudo engine/install.sh
#
# Installs the loader + eBPF object to /usr/lib/sluice, writes a root systemd unit that runs the
# engine at boot (RuntimeDirectory=/run/sluice, StateDirectory=/var/lib/sluice), and bakes in the
# invoking user's uid as the UI owner. Default-allow ⇒ no lockout risk; recovery is
# `sudo systemctl stop sluice-engine`. Uninstall: engine/uninstall.sh.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBJ="$DIR/ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
BIN="$DIR/loader/target/release/sluice-engine"
LIBDIR="/usr/lib/sluice"
UNIT="/etc/systemd/system/sluice-engine.service"

[[ $EUID -eq 0 ]] || { echo "Run as root: sudo $0" >&2; exit 1; }
OWNER_UID="${SUDO_UID:-0}"
for f in "$OBJ" "$BIN"; do
  [[ -f "$f" ]] || { echo "Missing $f — build first (see header)." >&2; exit 1; }
done

echo "[install] copying engine → $LIBDIR"
install -D -m 0755 "$BIN" "$LIBDIR/sluice-engine"
install -D -m 0644 "$OBJ" "$LIBDIR/sluice-ebpf"

# Install the canonical unit (same file the .deb ships) — static, with the owner uid in an
# EnvironmentFile so the unit itself is identical for source and packaged installs.
echo "[install] installing $UNIT"
install -D -m 0644 "$DIR/sluice-engine.service" "$UNIT"

echo "[install] writing owner uid ($OWNER_UID) → /etc/sluice/engine.env"
install -d -m 0755 /etc/sluice
printf 'SLUICE_OWNER_UID=%s\n' "$OWNER_UID" > /etc/sluice/engine.env
chmod 0644 /etc/sluice/engine.env

systemctl daemon-reload
echo "[install] done. Enable + start with:"
echo "    sudo systemctl enable --now sluice-engine"
echo "[install] then point Sluice at it (engine mode is the default)."

# Conflict note: Sluice and any other connection-firewall that hooks cgroup/connect both compete
# for the same kernel hook — running two at boot will fight.
echo
echo "[install] NOTE: if another connection-firewall is enabled (anything that hooks"
echo "          cgroup/connect), disable it so only Sluice manages connections."
