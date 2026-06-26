#!/usr/bin/env bash
# E0 spike runner — load the Sluice cgroup/connect eBPF observer and drain events.
#
# Loading/attaching BPF needs root, so this re-execs itself under sudo. It is ALLOW-ALL
# (observe only) and detaches on Ctrl-C, so there is no lockout risk at E0. Build first
# (see engine/README.md): the eBPF object and the loader binary must already exist.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OBJ="$DIR/ebpf/target/bpfel-unknown-none/release/sluice-ebpf"
BIN="$DIR/loader/target/release/sluice-engine"

if [[ ! -f "$OBJ" ]]; then echo "Missing eBPF object: $OBJ  (build it first)" >&2; exit 1; fi
if [[ ! -f "$BIN" ]]; then echo "Missing loader binary: $BIN  (build it first)" >&2; exit 1; fi

if [[ $EUID -ne 0 ]]; then
  echo "[run-e0] elevating (BPF load/attach needs root)…" >&2
  exec sudo SLUICE_BPF_OBJ="$OBJ" "$BIN" "$@"
fi
exec env SLUICE_BPF_OBJ="$OBJ" "$BIN" "$@"
