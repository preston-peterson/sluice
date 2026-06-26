#!/usr/bin/env bash
# E4 test (run as root). Verifies DNS-snoop hostname enrichment, no GUI: the engine should label
# a connection with the name the app resolved. Flushes the resolver cache so a fresh UDP/53 query
# is on the wire for the snoop to see, then resolves+connects to example.com and asserts the
# engine stream shows dst_host=example.com. Engine is default-allow ⇒ no lockout risk.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBJ="$DIR/ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
ENGINE="$DIR/loader/target/release/sluice-engine"
WATCH="$DIR/loader/target/release/sluice-watch"

[[ $EUID -eq 0 ]] || { echo "Run as root: sudo $0" >&2; exit 1; }
OWNER_UID="${SUDO_UID:-}"
[[ -n "$OWNER_UID" ]] || { echo "Need SUDO_UID (run via sudo)." >&2; exit 1; }
for f in "$OBJ" "$ENGINE" "$WATCH"; do [[ -f "$f" ]] || { echo "Missing $f — build first." >&2; exit 1; }; done

WORK="/tmp/sluice-e4-$$"; SOCK="$WORK/engine.sock"; RULES="$WORK/rules.json"
ELOG="$(mktemp)"; WLOG="$(mktemp)"; ENGINE_PID=""; WATCH_PID=""
cleanup() {
  [[ -n "$WATCH_PID" ]] && kill "$WATCH_PID" 2>/dev/null
  [[ -n "$ENGINE_PID" ]] && kill -INT "$ENGINE_PID" 2>/dev/null
  rm -rf "$WORK" "$ELOG" "$WLOG"
}
trap cleanup EXIT
asowner() { sudo -u "#$OWNER_UID" "$@"; }

echo "==================== E4 DNS-hostname test ===================="
mkdir -p "$WORK"; echo '[]' > "$RULES"
env SLUICE_BPF_OBJ="$OBJ" SLUICE_ENGINE_UDS="$SOCK" SLUICE_RULES="$RULES" SLUICE_OWNER_UID="$OWNER_UID" \
  "$ENGINE" >"$ELOG" 2>&1 &
ENGINE_PID=$!
sleep 1.5
kill -0 "$ENGINE_PID" 2>/dev/null || { echo "!! engine exited early:"; tail -15 "$ELOG"; exit 1; }
grep -q 'attached connect4' "$ELOG" || { echo "!! connect4 not attached:"; tail -20 "$ELOG"; exit 1; }
grep -q 'DNS snoop: capturing' "$ELOG" && echo "    DNS snoop up." || { echo "!! DNS snoop not running:"; tail -20 "$ELOG"; exit 1; }

echo ">>> watching the engine stream as the owner…"
timeout 12 sudo -u "#$OWNER_UID" env SLUICE_ENGINE_UDS="$SOCK" "$WATCH" >"$WLOG" 2>&1 &
WATCH_PID=$!
sleep 1.0
grep -q 'streaming' "$WLOG" || { echo "!! watch did not connect:"; cat "$WLOG"; exit 1; }

echo ">>> flushing resolver cache + resolving/connecting to example.com (twice to warm the cache)…"
resolvectl flush-caches 2>/dev/null || systemd-resolve --flush-caches 2>/dev/null || true
asowner curl -s --max-time 5 https://example.com >/dev/null 2>&1 || true
sleep 1
asowner curl -s --max-time 5 https://example.com >/dev/null 2>&1 || true
sleep 2
wait "$WATCH_PID" 2>/dev/null; WATCH_PID=""

echo
echo ">>> connection rows mentioning example.com:"
grep -i 'example.com' "$WLOG" | head -6 || true
echo
if grep -qi 'example.com' "$WLOG"; then
  echo "==================== PASS: connections labelled with the resolved hostname ===================="
  exit 0
else
  echo "==================== FAIL: no hostname enrichment ===================="
  echo "--- engine log (tail) ---"; tail -15 "$ELOG"
  echo "--- watch sample ---"; tail -15 "$WLOG"
  echo "(If this box uses DoH/DoT or nss-resolve-over-D-Bus with a warm cache, no plaintext"
  echo " UDP/53 was on the wire — that's the known E4 limit; SNI capture is E4.1.)"
  exit 1
fi
