# Sluice engine — cgroup/connect firewall (E0 observer → E1 enforcement)

This directory is the seed of the **Sluice enforcement engine** (DEC-010: build our own in-kernel engine)
engine). It is intentionally **separate from the `crates/` sluice workspace** —
each crate here is its own detached cargo workspace, because the eBPF crate builds for a
different target (`bpfel-unknown-none`) on nightly, while the rest of Sluice is pinned to stable
1.89.

## What this proves

The engine exists to avoid the **seconds** of per-connection browsing latency added by a
synchronous "ask the UI per new connection" model in the packet hot path.

- **E0 (de-risk gate — ✅ passed):** a `cgroup/connect{4,6}` eBPF program observes **every**
  outbound connection in-kernel and pushes a record to userspace over a **ring buffer**,
  asynchronously — **~1µs added latency** (vs seconds for a userspace per-connection ask). See the latency gate below.
- **E1 (enforcement):** `connect4` now consults a `BLOCKLIST` rule map (dst IPv4 + port) and
  **denies** matching connections in-kernel (returns 0 → `connect()` gets EPERM). A userspace
  control plane loads rules from a file and reconciles the map live on `SIGHUP`. Default-allow:
  an empty map blocks nothing. (v6/CIDR/by-app enforcement are later phases.)
- **E3.0 (UI link):** the engine is also a **gRPC server** on a hardened UDS; it streams each
  observed connection (enriched engine-side with `/proc` process info) to the **Sluice UI**, which
  connects as a client and shows them in the existing feed. Engine = server (the inverse of a UI-as-server design).

```
connect(2) ─ cgroup/connect4 (eBPF: BLOCKLIST? deny : allow; emit) ─┬─ verdict to kernel
                                                                     └─ringbuf─► sluice-engine (root)
                                                                                   ├─ rules (SIGHUP)
                                                                                   └─gRPC/UDS─► Sluice UI feed
```

## Layout

| Crate | Target | Role |
|---|---|---|
| `common/` | both | `#[repr(C)]` `ConnEvent` ABI + `rule_key4` packing, shared kernel↔userspace (`no_std`) |
| `ebpf/`   | `bpfel-unknown-none` (nightly) | the eBPF program: `connect4`/`connect6` + `EVENTS` ring buffer + `BLOCKLIST` map |
| `loader/` | host (`sluice-engine`) | loads the object, attaches to the cgroup-v2 root, drives `BLOCKLIST`, async-drains events, serves the gRPC UI link. Also builds `sluice-watch` (a no-GUI link client). |
| `../crates/sluice-proto` | host | generated gRPC stubs for the engine↔UI contract (`sluice.proto`), shared with `sluice-ui` |

## Prerequisites (already set up on this host)

- nightly toolchain + `rust-src` component (for `-Z build-std=core`)
- `bpf-linker` on `PATH` — install with `cargo +stable install bpf-linker --locked`
  (the repo's 1.89 pin is too old to build it; `+stable` sidesteps the `rust-toolchain.toml`)
- a cgroup-v2 unified hierarchy at `/sys/fs/cgroup` (`stat -fc %T` → `cgroup2fs`)
- kernel BTF at `/sys/kernel/btf/vmlinux`

## Build

```sh
# 1) the eBPF object (uses ebpf/.cargo/config.toml → bpfel target + build-std; nightly pin)
( cd ebpf && cargo build --release )
# 2) the userspace loader (host target, stable is fine)
( cd loader && cargo build --release )
```

## Run (needs root — BPF load/attach)

```sh
./run-e0.sh                                       # observe only; Ctrl-C to detach
SLUICE_RULES=/tmp/rules.json ./run-e0.sh          # enforce the rules in that JSON store
```

The **rule store** is a JSON array of `{ "ip": "<ipv4>", "port": <port> }` (`port` 0 / omitted =
any port to that IP). Default path `/var/lib/sluice/rules.json` (override with `SLUICE_RULES`);
loaded at startup and on `SIGHUP`, and rewritten by the decision RPCs (E3.1). Default-allow (empty
store blocks nothing) + auto-detach on exit ⇒ no lockout risk. RPC-set rules pass a safelist
(never block loopback / DNS-stub / `0.0.0.0`); rules edited directly into the root-owned file are
trusted as-is.

## Latency gate (E0)

```sh
python3 bench-connect.py 5000     # 1) BASELINE: loader NOT running
# …start ./run-e0.sh in another terminal…
python3 bench-connect.py 5000     # 2) ATTACHED: loader running
```

The delta in p50/p99 connect(2) latency is the hook's added cost. **Gate: low microseconds**
(nothing like the seconds a userspace per-connection ask adds). `./measure-e0.sh` does both passes in one root run.

## Enforcement test (E1)

```sh
sudo ./test-e1.sh     # self-contained: blocks a loopback IP:PORT, proves deny + control + reload
```

It blocks `127.0.0.1:39991`, asserts a connect there is **DENIED** (EPERM) while `:39992` still
works, then removes the rule + `SIGHUP`s and asserts the port flows again. No external network,
no collateral on other services (IP+port exact match).

## Decision-RPC test (E3.1)

```sh
sudo ./test-e3.1.sh   # block/allow via the gRPC RPCs (sluice-rule client), no GUI
```

Blocks `1.1.1.1:443` via `SetRule` → that connect is **DENIED** while `1.0.0.1:443` flows;
`ListRules` shows it; **restarts the engine** to prove the rule persists; `RemoveRule` un-blocks;
and the safelist refuses `block 127.0.0.1:53`. The `sluice-rule` CLI (`block`/`remove`/`list`) and
`sluice-watch` (stream) are the no-GUI clients for the engine.

## UI link (E3.0)

Run the engine with a UI socket, then point Sluice at it:

```sh
# engine (root): serve the UI link. SLUICE_OWNER_UID falls back to SUDO_UID under sudo.
sudo SLUICE_ENGINE_UDS=/run/sluice/engine.sock SLUICE_OWNER_UID=$(id -u) ./run-e0.sh
# Sluice UI (your user): source the feed from the engine.
SLUICE_ENGINE_UDS=/run/sluice/engine.sock sluice-ui
```

No-GUI verification of the gRPC/UDS link:

```sh
sudo ./test-e3.sh     # engine + unprivileged sluice-watch client; asserts events stream through
# or, manually, while the engine runs:
SLUICE_ENGINE_UDS=/run/sluice/engine.sock ./loader/target/release/sluice-watch
```

The link is local-only (UDS), reachable solely by the owner uid (chown `0600` + `SO_PEERCRED`).
Read-only at E3.0 — the UI shows the feed but can't yet write rules (that's E3.1).

## Recovery

Default-allow means no lockout risk. If anything seems off, Ctrl-C the loader (it detaches the
programs) — or the cgroup programs auto-detach when the loader process exits.
