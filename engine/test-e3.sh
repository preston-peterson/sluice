#!/usr/bin/env bash
# E3.0 link test (run as root). Verifies the engine→UI gRPC stream over the hardened UDS
# WITHOUT the GUI: starts the engine with a UI link, connects the *unprivileged* sluice-watch
# client as the owner (exercising the chown 0600 + SO_PEERCRED path), generates traffic, and
# asserts connection events stream through. Engine is default-allow ⇒ no lockout risk.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBJ="$DIR/ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
ENGINE="$DIR/loader/target/release/sluice-engine"
WATCH="$DIR/loader/target/release/sluice-watch"

[[ $EUID -eq 0 ]] || { echo "Run as root: sudo $0" >&2; exit 1; }
OWNER_UID="${SUDO_UID:-}"
[[ -n "$OWNER_UID" ]] || { echo "Need SUDO_UID (run via sudo, not as a root login)." >&2; exit 1; }
for f in "$OBJ" "$ENGINE" "$WATCH"; do [[ -f "$f" ]] || { echo "Missing $f — build first." >&2; exit 1; }; done

SOCKDIR="/tmp/sluice-e3-$$"; SOCK="$SOCKDIR/engine.sock"
ELOG="$(mktemp)"; WLOG="$(mktemp)"
ENGINE_PID=""; WATCH_PID=""
cleanup() {
  [[ -n "$WATCH_PID" ]] && kill "$WATCH_PID" 2>/dev/null
  [[ -n "$ENGINE_PID" ]] && kill -INT "$ENGINE_PID" 2>/dev/null
  rm -rf "$SOCKDIR" "$ELOG" "$WLOG"
}
trap cleanup EXIT
asowner() { sudo -u "#$OWNER_UID" "$@"; }

echo "==================== E3.0 engine→UI link test ===================="
echo ">>> starting engine (UI link on $SOCK, owner uid $OWNER_UID)…"
env SLUICE_BPF_OBJ="$OBJ" SLUICE_ENGINE_UDS="$SOCK" SLUICE_OWNER_UID="$OWNER_UID" "$ENGINE" >"$ELOG" 2>&1 &
ENGINE_PID=$!
sleep 1.5
if ! kill -0 "$ENGINE_PID" 2>/dev/null; then echo "!! engine exited early:"; cat "$ELOG"; exit 1; fi
grep -q 'UI link: gRPC' "$ELOG" && echo "    engine UI link up." || { echo "!! no UI link banner:"; cat "$ELOG"; exit 1; }
grep -q 'attached connect4' "$ELOG" || { echo "!! connect4 did NOT attach (eBPF verifier?):"; cat "$ELOG"; exit 1; }

echo ">>> connecting unprivileged sluice-watch as the owner…"
# Inline sudo (not the asowner function): `timeout` execs an external program, not a shell fn.
timeout 6 sudo -u "#$OWNER_UID" env SLUICE_ENGINE_UDS="$SOCK" "$WATCH" >"$WLOG" 2>&1 &
WATCH_PID=$!
sleep 1.0
grep -q 'streaming' "$WLOG" || { echo "!! watch did not connect:"; cat "$WLOG"; exit 1; }

echo ">>> generating traffic (owner curls)…"
asowner curl -s --max-time 3 https://1.1.1.1 >/dev/null 2>&1 || true
asowner curl -s --max-time 3 https://example.com >/dev/null 2>&1 || true
sleep 2
wait "$WATCH_PID" 2>/dev/null; WATCH_PID=""

count=$(grep -c '^#' "$WLOG" || true)
echo
echo ">>> events the UI client received over gRPC/UDS:"
grep '^#' "$WLOG" | head -8
echo "    …"
echo "    total: $count"
echo
if [[ "$count" -ge 1 ]]; then
  echo "==================== PASS: link streams connection events ===================="
  exit 0
else
  echo "==================== FAIL: no events received ===================="
  echo "--- engine log ---"; tail -15 "$ELOG"
  echo "--- watch log ---"; cat "$WLOG"
  exit 1
fi
