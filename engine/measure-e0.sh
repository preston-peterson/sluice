#!/usr/bin/env bash
# E0 one-shot gate (run as root). Does the whole measurement and exits — no Ctrl-C needed:
#   1. BASELINE  — connect(2) bench with no BPF attached
#   2. attach the cgroup/connect observer (allow-all + ringbuf)
#   3. ATTACHED  — same bench while attached  → delta = the hook's added latency
#   4. proof     — show a sample of the events the observer captured, and the total count
#   5. detach    — stop the observer
#
# Allow-all + auto-detach ⇒ no lockout risk. For the cleanest delta, stop any other connection-firewall first
# (its own hooks add a constant offset to both passes, so the delta is still valid either way).
set -uo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBJ="$DIR/ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
BIN="$DIR/loader/target/release/sluice-engine"
N="${1:-5000}"

if [[ $EUID -ne 0 ]]; then echo "Run as root: sudo $0" >&2; exit 1; fi
for f in "$OBJ" "$BIN"; do [[ -f "$f" ]] || { echo "Missing $f — build first." >&2; exit 1; }; done

echo "==================== E0 latency gate (N=$N) ===================="
echo
echo ">>> [1/3] BASELINE — no BPF attached:"
python3 "$DIR/bench-connect.py" "$N"
echo

LOG="$(mktemp)"
echo ">>> attaching observer (events -> $LOG)…"
env SLUICE_BPF_OBJ="$OBJ" "$BIN" >"$LOG" 2>&1 &
PID=$!
sleep 1.5
if ! kill -0 "$PID" 2>/dev/null; then echo "!! observer exited early:"; cat "$LOG"; exit 1; fi
grep -q 'draining' "$LOG" && echo "    attached OK." || echo "    (attached; no banner yet)"
echo
echo ">>> [2/3] ATTACHED — observer running:"
python3 "$DIR/bench-connect.py" "$N"
echo
echo ">>> [3/3] PROOF — sample of observed connection events:"
sleep 0.3
grep '^#' "$LOG" | head -6
echo "    …"
echo "    total events observed during run: $(grep -c '^#' "$LOG")"
echo
kill -INT "$PID" 2>/dev/null
wait "$PID" 2>/dev/null
rm -f "$LOG"
echo "==================== detached — E0 done ===================="
