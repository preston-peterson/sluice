#!/usr/bin/env bash
# E6.1 inbound-enforcement test (run as root). Proves the nftables input chain works using a
# network namespace as a simulated REMOTE peer (loopback is always exempt, so it can't be used):
#   - a NEW inbound to a NON-allowed port is DROPPED; to an ALLOWED port it connects.
#   - the host's own outbound return traffic keeps working (established,related accept).
#   - turning enforcement off (and stopping the engine) removes the table → inbound reopens.
#
# ⚠ While this runs, the host briefly enforces default-deny INBOUND (active SSH survives via
# established; the allow-list includes 22). It tears everything down on exit.
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBJ="$DIR/ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
ENGINE="$DIR/loader/target/release/sluice-engine"
RULE="$DIR/loader/target/release/sluice-rule"
PROBE="$DIR/e1-probe.py"

[[ $EUID -eq 0 ]] || { echo "Run as root: sudo $0" >&2; exit 1; }
OWNER_UID="${SUDO_UID:-}"
[[ -n "$OWNER_UID" ]] || { echo "Need SUDO_UID (run via sudo)." >&2; exit 1; }
command -v nft >/dev/null || { echo "nft not found (install nftables)." >&2; exit 1; }
command -v ip >/dev/null || { echo "ip not found." >&2; exit 1; }
for f in "$OBJ" "$ENGINE" "$RULE" "$PROBE"; do [[ -f "$f" ]] || { echo "Missing $f — build first." >&2; exit 1; }; done

NS=sluice-e6; HOSTIP=10.123.0.1; PEERIP=10.123.0.2
ALLOWED=39996; BLOCKED=39995
WORK="/tmp/sluice-e6-$$"; SOCK="$WORK/engine.sock"; INB="$WORK/inbound.json"
ELOG="$(mktemp)"; PIDS=(); ENGINE_PID=""
cleanup() {
  [[ -n "$ENGINE_PID" ]] && kill -INT "$ENGINE_PID" 2>/dev/null; wait "$ENGINE_PID" 2>/dev/null
  for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null; done
  ip netns del "$NS" 2>/dev/null
  ip link del veth-host 2>/dev/null
  nft delete table inet sluice 2>/dev/null
  rm -rf "$WORK" "$ELOG"
}
trap cleanup EXIT
asowner() { sudo -u "#$OWNER_UID" "$@"; }
nsprobe() { ip netns exec "$NS" python3 "$PROBE" probe "$1" "$HOSTIP"; }

pass=0; fail=0
check() { if [[ "$2" == "$3" ]]; then echo "    PASS: $1 ($3)"; pass=$((pass+1)); else echo "    FAIL: $1 — expected $2, got $3"; fail=$((fail+1)); fi; }
check_ne() { if [[ "$2" != "$3" ]]; then echo "    PASS: $1 ($3)"; pass=$((pass+1)); else echo "    FAIL: $1 — got $3 (should differ from $2)"; fail=$((fail+1)); fi; }

echo "==================== E6.1 inbound-enforcement test ===================="
echo ">>> setting up a netns 'remote' peer ($PEERIP → host $HOSTIP)…"
ip link add veth-host type veth peer name veth-ns
ip addr add "$HOSTIP/24" dev veth-host; ip link set veth-host up
ip netns add "$NS"
ip link set veth-ns netns "$NS"
ip netns exec "$NS" ip addr add "$PEERIP/24" dev veth-ns
ip netns exec "$NS" ip link set veth-ns up
ip netns exec "$NS" ip link set lo up

echo ">>> starting listeners on $ALLOWED + $BLOCKED (host, all interfaces)…"
python3 "$PROBE" listen "$ALLOWED" & PIDS+=($!)
python3 "$PROBE" listen "$BLOCKED" & PIDS+=($!)
sleep 0.4

mkdir -p "$WORK"; echo '{}' > "$INB"
env SLUICE_BPF_OBJ="$OBJ" SLUICE_ENGINE_UDS="$SOCK" SLUICE_INBOUND="$INB" SLUICE_OWNER_UID="$OWNER_UID" \
  "$ENGINE" >"$ELOG" 2>&1 &
ENGINE_PID=$!
sleep 1.5
kill -0 "$ENGINE_PID" 2>/dev/null || { echo "!! engine exited early:"; tail -15 "$ELOG"; exit 1; }

echo
echo ">>> [1/4] ENFORCE on (allow tcp:22 + tcp:$ALLOWED; NOT $BLOCKED):"
asowner env SLUICE_ENGINE_UDS="$SOCK" "$RULE" inbound-set on "tcp:22" "tcp:$ALLOWED" >/dev/null
sleep 0.3
nft list table inet sluice >/dev/null 2>&1 && echo "    nft table present." || { echo "!! table missing:"; tail "$ELOG"; exit 1; }
check    "allowed port $ALLOWED connects from the peer" "OK" "$(nsprobe $ALLOWED)"
check_ne "blocked port $BLOCKED is dropped"             "OK" "$(nsprobe $BLOCKED)"

echo
echo ">>> [2/4] host outbound still works (established,related accept):"
check "curl example.com succeeds" "OK" "$(asowner curl -s -o /dev/null -w OK --max-time 6 https://example.com || echo FAIL)"

echo
echo ">>> [3/4] ENFORCE off → table removed, blocked port reopens:"
asowner env SLUICE_ENGINE_UDS="$SOCK" "$RULE" inbound-set off >/dev/null
sleep 0.3
check "$BLOCKED now connects" "OK" "$(nsprobe $BLOCKED)"

echo
echo ">>> [4/4] teardown on engine stop:"
asowner env SLUICE_ENGINE_UDS="$SOCK" "$RULE" inbound-set on "tcp:$ALLOWED" >/dev/null
sleep 0.3
kill -INT "$ENGINE_PID" 2>/dev/null; wait "$ENGINE_PID" 2>/dev/null; ENGINE_PID=""
sleep 0.3
nft list table inet sluice >/dev/null 2>&1 && { echo "    FAIL: table still present after stop"; fail=$((fail+1)); } || { echo "    PASS: table removed on stop"; pass=$((pass+1)); }

echo
echo "==================== result: $pass passed, $fail failed ===================="
exit $(( fail > 0 ? 1 : 0 ))
