# Sluice

A polished, modern **application firewall and network monitor** for Ubuntu (GNOME).
Sluice watches and controls **both outbound and inbound** connections on your machine, with a
live decision feed and a two-click allow/block workflow.

Why it exists: on a typical desktop it's *hard to see what's connecting out* and *hard to find
and allow the traffic you actually want*. Sluice puts that front-and-center.

## Architecture

Sluice is two parts with a clean privilege split:

```
┌─ sluice-ui (unprivileged, your user) ─┐        ┌─ sluice-engine (root systemd service) ─┐
│  Tauri desktop app · live feed ·      │ gRPC   │  eBPF cgroup/connect (outbound) ·       │
│  allow/block · history (SQLite)       │◄──────►│  nftables + conntrack (inbound) ·       │
│  gRPC CLIENT                          │  UDS   │  passive DNS snoop · gRPC SERVER        │
└───────────────────────────────────────┘        └────────────────────────────────────────┘
                                                   enforces in-kernel; streams events to the UI
```

- **Engine** (`engine/`): a small **root daemon** that does the actual enforcement — an
  `cgroup/connect` eBPF program denies outbound connections from an in-kernel rule map (~1µs per
  connection), an `nftables` table gates inbound, and a passive DNS snoop + `/proc`/container
  enrichment label the events. It serves the UI over a hardened Unix socket (`0600`,
  `SO_PEERCRED`-gated to the owner).
- **UI** (`crates/sluice-ui`): an **unprivileged Tauri app** — a gRPC *client* of the engine. It
  renders the feed, writes rules (the root engine does the privileged work), and keeps local
  history. It never runs as root and makes no network calls of its own.

## What it does

- **Feed** — coalesced live activity with free-text + verdict + protocol/category filters, a
  **time-window** view (live / hour / today / 7d), expandable per-row detail (pid/uid, src/dst,
  protocol, destination country via offline geoIP, on-demand rDNS), a **process explainer**
  (local-only), and **project grouping** by launching app.
- **Act** — **two-click allow/block** with a scope × duration picker, confirmation, and a
  critical-host safelist; a firewall-rules panel; outbound rules enforced in-kernel for **IPv4 and
  IPv6**.
- **Views** — **Apps** and **Destinations** permission panels (with real rule posture), **Usage**
  top-talkers, a **Security** event log, and **Inbound** control (observe ↔ enforce + an
  allow-list, with per-port drill-down).
- **Integration** — system tray, desktop notifications, engine-status indicator, a live bandwidth
  graph, and **persisted history** (SQLite) so the feed survives restarts.
- **Safe by default** — the default posture is monitor (never holds traffic); the engine is
  default-allow + denylist, so there's no lockout risk. Recovery is always
  `sudo systemctl stop sluice-engine`.

## Repo layout

| Path | What |
|---|---|
| `crates/sluice-ui` | The Tauri desktop app (unprivileged gRPC client) — the feed + allow/block UI. |
| `crates/sluice-types` | UI-agnostic value types (feed/verdict/decision) shared by the UI. |
| `crates/sluice-proto` | The engine↔UI gRPC contract (`sluice.proto`) + generated stubs. |
| `engine/loader` | The root engine daemon: loads/attaches the eBPF, rule store, inbound nftables, gRPC server. |
| `engine/ebpf` | The `cgroup/connect{4,6}` eBPF program (built on nightly + `bpf-linker`). |
| `engine/common` | The shared eBPF↔loader ABI. |
| `scripts/`, `justfile` | Dev bootstrap + common tasks. |

## Install

One command builds and installs both halves (the root engine service + the desktop UI), pulling
in the build toolchain as needed:

```bash
./install.sh
```

Then launch **Sluice** from your app menu (or run `sluice-ui`). The engine runs as a root systemd
service (`sluice-engine`); the UI is per-user and unprivileged. Recovery is always
`sudo systemctl stop sluice-engine`. To remove everything: `./uninstall.sh`. See
[`docs/INSTALL.md`](docs/INSTALL.md) for details and options.

## Development

```bash
just setup          # Rust toolchain + protoc + Tauri/WebKitGTK libs
just check          # fmt + clippy + tests
just engine-build   # build the eBPF object + the root loader (needs nightly + bpf-linker)
just ui             # run the desktop app against a running engine
```

## License

Sluice's code is currently **unlicensed** pending a licensing decision — see
[`LICENSE`](LICENSE).
