#!/usr/bin/env bash
# E1 enforcement test (run as root). Self-contained and safe — it blocks one loopback
# IP:PORT and proves in-kernel enforcement with zero collateral:
#   1. ENFORCE — a connect to the blocked 127.0.0.1:PORT is DENIED (EPERM); a connect to a
#      different loopback port still works.
#   2. RELOAD  — remove the rule + SIGHUP; the once-blocked port now connects (live mgmt).
# Default-allow ⇒ no lockout risk; the loader auto-detaches on exit.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBJ="$DIR/ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
BIN="$DIR/loader/target/release/sluice-engine"
PROBE="$DIR/e1-probe.py"
BLOCK_PORT=39991
OK_PORT=39992

if [[ $EUID -ne 0 ]]; then echo "Run as root: sudo $0" >&2; exit 1; fi
for f in "$OBJ" "$BIN" "$PROBE"; do [[ -f "$f" ]] || { echo "Missing $f — build first." >&2; exit 1; }; done

RULES="$(mktemp)"; LOG="$(mktemp)"
# JSON rule store (the engine's canonical format). Loopback is allowed via the file (root-
# trusted); the safelist only guards the UI/RPC path.
printf '[{"ip":"127.0.0.1","port":%s}]\n' "$BLOCK_PORT" > "$RULES"
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done; rm -f "$RULES" "$LOG"; }
trap cleanup EXIT

echo "==================== E1 enforcement test ===================="
echo ">>> starting listeners on $BLOCK_PORT (to-be-blocked) and $OK_PORT (control)…"
python3 "$PROBE" listen "$BLOCK_PORT" & PIDS+=($!)
python3 "$PROBE" listen "$OK_PORT" & PIDS+=($!)
sleep 0.4

echo ">>> starting engine with rule: 127.0.0.1:$BLOCK_PORT blocked"
env SLUICE_BPF_OBJ="$OBJ" SLUICE_RULES="$RULES" "$BIN" >"$LOG" 2>&1 & ENGINE=$!; PIDS+=("$ENGINE")
sleep 1.5
if ! kill -0 "$ENGINE" 2>/dev/null; then echo "!! engine exited early:"; cat "$LOG"; exit 1; fi
grep -q 'draining' "$LOG" && echo "    attached + rule loaded." || { echo "!! engine not ready:"; cat "$LOG"; exit 1; }
# Catch a verifier failure on connect4 (v4 enforcement) loudly rather than silently passing v4.
grep -q 'attached connect4' "$LOG" || { echo "!! connect4 did NOT attach (eBPF verifier?):"; cat "$LOG"; exit 1; }

pass=0; fail=0
check() { # desc expected actual
  if [[ "$2" == "$3" ]]; then echo "    PASS: $1 ($3)"; pass=$((pass+1)); else echo "    FAIL: $1 — expected $2, got $3"; fail=$((fail+1)); fi
}

echo
echo ">>> [1/2] ENFORCE:"
check "connect to blocked :$BLOCK_PORT is denied" "DENIED:PermissionError" "$(python3 "$PROBE" probe "$BLOCK_PORT")"
check "connect to control :$OK_PORT works"        "OK"                      "$(python3 "$PROBE" probe "$OK_PORT")"

echo
echo ">>> [2/2] RELOAD (remove rule + SIGHUP):"
echo '[]' > "$RULES"   # empty the rules file
kill -HUP "$ENGINE"
sleep 0.6
check "once-blocked :$BLOCK_PORT now works" "OK" "$(python3 "$PROBE" probe "$BLOCK_PORT")"

echo
echo ">>> engine log (blocked events tagged BLOCK):"
grep -E 'BLOCK|block|unblock|active' "$LOG" | head -12

echo
echo "==================== result: $pass passed, $fail failed ===================="
exit $(( fail > 0 ? 1 : 0 ))
