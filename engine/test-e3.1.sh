#!/usr/bin/env bash
# E3.1 test (run as root). Exercises the decision RPCs over gRPC/UDS, no GUI: block/allow a
# destination from the (unprivileged) sluice-rule client, prove in-kernel enforcement, that the
# rule PERSISTS across an engine restart, and that the safelist refuses a loopback rule.
# Engine is default-allow ⇒ no lockout risk. Brief external connects to 1.1.1.1 / 1.0.0.1:443.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBJ="$DIR/ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
ENGINE="$DIR/loader/target/release/sluice-engine"
RULE="$DIR/loader/target/release/sluice-rule"
PROBE="$DIR/e1-probe.py"

[[ $EUID -eq 0 ]] || { echo "Run as root: sudo $0" >&2; exit 1; }
OWNER_UID="${SUDO_UID:-}"
[[ -n "$OWNER_UID" ]] || { echo "Need SUDO_UID (run via sudo)." >&2; exit 1; }
for f in "$OBJ" "$ENGINE" "$RULE" "$PROBE"; do [[ -f "$f" ]] || { echo "Missing $f — build first." >&2; exit 1; }; done

WORK="/tmp/sluice-e31-$$"; SOCK="$WORK/engine.sock"; RULES="$WORK/rules.json"
ELOG="$(mktemp)"; ENGINE_PID=""
cleanup() { [[ -n "$ENGINE_PID" ]] && kill -INT "$ENGINE_PID" 2>/dev/null; rm -rf "$WORK" "$ELOG"; }
trap cleanup EXIT
asowner() { sudo -u "#$OWNER_UID" "$@"; }
rulecli() { asowner env SLUICE_ENGINE_UDS="$SOCK" "$RULE" "$@"; }
probe()   { asowner env python3 "$PROBE" probe "$1" "$2"; }

start_engine() {
  env SLUICE_BPF_OBJ="$OBJ" SLUICE_ENGINE_UDS="$SOCK" SLUICE_RULES="$RULES" SLUICE_OWNER_UID="$OWNER_UID" \
    "$ENGINE" >>"$ELOG" 2>&1 &
  ENGINE_PID=$!
  sleep 1.5
  kill -0 "$ENGINE_PID" 2>/dev/null || { echo "!! engine exited early:"; tail -15 "$ELOG"; exit 1; }
  # Catch a verifier failure on connect4 loudly (v4 enforcement would silently no-op otherwise).
  grep -q 'attached connect4' "$ELOG" || { echo "!! connect4 did NOT attach (eBPF verifier?):"; tail -20 "$ELOG"; exit 1; }
}
stop_engine() { kill -INT "$ENGINE_PID" 2>/dev/null; wait "$ENGINE_PID" 2>/dev/null; ENGINE_PID=""; }

pass=0; fail=0
check() { if [[ "$2" == "$3" ]]; then echo "    PASS: $1 ($3)"; pass=$((pass+1)); else echo "    FAIL: $1 — expected $2, got $3"; fail=$((fail+1)); fi; }

echo "==================== E3.1 decision-RPC test ===================="
mkdir -p "$WORK"; echo '[]' > "$RULES"   # root-owned 0755 dir; engine chowns the socket to the owner
start_engine
echo "    engine up (rules: $RULES)"

echo
echo ">>> [1/4] BLOCK via RPC, then enforce:"
check "set_rule(block 1.1.1.1:443) acked" "block 1.1.1.1:443 -> ok=true " "$(rulecli block 1.1.1.1 443)"
check "connect 1.1.1.1:443 denied"        "DENIED:PermissionError"        "$(probe 443 1.1.1.1)"
check "connect 1.0.0.1:443 (unblocked) ok" "OK"                            "$(probe 443 1.0.0.1)"

echo
echo ">>> [2/4] ListRules shows it:"
check "list shows the rule" "v4:1.1.1.1:443  block 1.1.1.1:443" "$(rulecli list)"

echo
echo ">>> [3/4] PERSISTENCE across an engine restart:"
stop_engine
echo "    (engine restarted)"; start_engine
check "rule survived restart (still denied)" "DENIED:PermissionError" "$(probe 443 1.1.1.1)"

echo
echo ">>> [4/4] REMOVE via RPC + safelist guard:"
check "remove acked" "remove v4:1.1.1.1:443 -> ok=true " "$(rulecli remove v4:1.1.1.1:443)"
check "1.1.1.1:443 now allowed" "OK" "$(probe 443 1.1.1.1)"
check "safelist refuses blocking loopback" "block 127.0.0.1:53 -> ok=false refused: loopback (would strand local services and the DNS stub)" "$(rulecli block 127.0.0.1 53)"

echo
echo "==================== result: $pass passed, $fail failed ===================="
exit $(( fail > 0 ? 1 : 0 ))
