# Architecture

A tour of how Sluice is put together, written for someone who has landed on the repo and wants to understand the system before reading code — or decide whether it's worth their time.

Sluice is an **application firewall and network monitor for Ubuntu (GNOME)**. It shows you, in real time, every outbound connection your machine makes — which process, to which host, allowed or blocked — and lets you block or allow a destination in two clicks. It also watches and (optionally) filters inbound connections.

The whole product is built around a single deliberate split: a **small privileged engine** that does the kernel work, and an **unprivileged desktop app** that does everything else. Understanding that split is most of understanding Sluice.

## The big picture

```
                          ┌──────────────────── your machine ─────────────────────────┐
                          │                                                            │
  any process             │   ┌───────────── kernel ─────────────┐                     │
  calls connect(2) ───────┼──▶│  eBPF cgroup/connect4 + connect6  │                     │
                          │   │  • check BLOCKLIST / BLOCKLIST6    │── verdict ──▶ allow │
                          │   │    (in-kernel deny, ~1µs)          │               /deny │
                          │   │  • emit event to ring buffer       │                     │
                          │   └───────────────┬───────────────────┘                     │
                          │                   │ ring buffer (async drain)               │
                          │                   ▼                                          │
   inbound conntrack ─────┼──▶ ┌─────────────────────────────────┐                      │
   NEW events             │    │   sluice-engine  (ROOT daemon)   │                      │
                          │    │   • loads/attaches eBPF          │                      │
   nftables inet sluice ◀─┼────│   • rule store → BLOCKLIST maps  │                      │
   (inbound enforcement)  │    │   • /var/lib/sluice/rules.json   │                      │
                          │    │   • DNS snoop (AF_PACKET)        │                      │
   AF_PACKET DNS ─────────┼──▶ │   • /proc + Docker enrichment    │                      │
                          │    │   • gRPC SERVER on a UDS         │                      │
                          │    └───────────────┬─────────────────┘                      │
                          │                    │ gRPC over /run/sluice/engine.sock       │
                          │                    │ (0700 dir / 0600 sock, SO_PEERCRED)     │
                          │                    ▼                                         │
                          │    ┌─────────────────────────────────┐                      │
                          │    │   sluice-ui  (UNPRIVILEGED app)  │                      │
                          │    │   • gRPC CLIENT of the engine    │     ┌──────────────┐ │
                          │    │   • live feed + views            │────▶│ desktop / tray│ │
                          │    │   • writes rules (engine acts)   │     │ + notifications│ │
                          │    │   • SQLite history (~/.local)    │     └──────────────┘ │
                          │    └─────────────────────────────────┘                      │
                          └────────────────────────────────────────────────────────────┘
```

Two processes, one socket between them. The engine runs as `root` and is the only thing that touches the kernel. The UI runs as your normal user, holds no privilege, and asks the engine to do anything that matters. Everything the engine learns about a connection it streams to the UI; everything the UI decides it sends back as a rule. There is no other coupling.

## The privilege boundary

This is the single most important design decision in Sluice, so it's worth being explicit.

| | Engine (`sluice-engine`) | UI (`sluice-ui`) |
|---|---|---|
| **Runs as** | root (systemd service) | your user (desktop app) |
| **Touches the kernel** | yes — loads eBPF, programs nftables | never |
| **Holds rules** | yes — the authoritative store | no — sends requests only |
| **Network egress** | none | none (except explicit, user-clicked lookups) |
| **State on disk** | `/var/lib/sluice` (0700, root) | `~/.local/share/sluice` (0700, user) |
| **Role on the wire** | gRPC **server** | gRPC **client** |

The privileged surface is kept as small as possible: load and attach a couple of eBPF programs, maintain two kernel maps, manage one nftables table, read `/proc`, and serve a local socket. Nothing in that list requires the engine to make a network connection, render UI, or run untrusted input. The large, fast-moving, feature-rich half — the desktop app — has no privilege at all. A bug in the UI cannot, by construction, edit the kernel; it can only ask the engine to, and the engine validates every request.

If anything ever goes wrong, recovery is always the same one line:

```sh
sudo systemctl stop sluice-engine
```

Stopping the engine detaches the eBPF programs and removes the nftables table, so the network opens back up immediately. The engine is **default-allow**: it only denies destinations that are explicitly on its blocklist, so it can never lock you out of your own machine even while running.

## The engine

`sluice-engine` is a small async (tokio) Rust daemon that lives in `engine/loader`. Its companion crates are `engine/ebpf` (the in-kernel program) and `engine/common` (the byte-exact ABI the two share). It does six jobs.

### 1. Load and attach the eBPF programs

At startup the loader reads the compiled BPF object and attaches two programs — `connect4` and `connect6` — to the **root of the cgroup-v2 hierarchy** (`/sys/fs/cgroup`). Attaching at the root means every task on the machine is covered: anything that calls `connect(2)`, in any cgroup, traverses the hook. The attach uses the cgroup *link* API, so the programs auto-detach cleanly when the process exits — there is no leaked-state failure mode where the engine dies and leaves the kernel half-configured.

### 2. The outbound data path (eBPF `cgroup/connect`)

This is the hot path, and it is the reason Sluice has its own engine. The program in `engine/ebpf/src/main.rs` runs **inside the kernel** at the moment a socket connects:

1. Read the connecting task's PID and UID, the destination address and port, and the L4 protocol — all as constant-offset reads from the socket-address context (the only form the eBPF verifier permits here).
2. Pack the destination into a map key and look it up in the **blocklist**:
   - IPv4 destinations key into `BLOCKLIST` — a `u64` packing the network-order address and port.
   - IPv6 destinations key into `BLOCKLIST6` — a 20-byte struct key (four address words + port), because a 128-bit address won't fit a `u64`.
   - Each map is checked twice: once for an exact `(address, port)` rule, once for an `(address, any-port)` rule (port `0`). Presence in the map means **deny**.
3. Stamp the connection's verdict (`allow` / `block`) onto a fixed-layout `ConnEvent` record.
4. Emit that record to a 256 KiB **ring buffer** for userspace to drain asynchronously.
5. Return the verdict to the kernel: allow lets `connect(2)` proceed; deny makes it return `EPERM`.

The decision is made entirely in-kernel from a map lookup. There is no packet copy, no holding the connection, and no round-trip to userspace on the decision path. The measured overhead is on the order of **~1µs per connection** — small enough that it's invisible in practice, even on a busy machine making thousands of connections a second. The feed (the userspace event stream) is decoupled from enforcement: the kernel never waits for the UI to drain the ring buffer.

`engine/common` defines `ConnEvent`, `Key6`, and the two key-packing functions (`rule_key4`, `rule_key6`) as a single, `#[repr(C)]`, `no_std` source of truth shared by both the kernel program and the loader. That shared definition matters: if the two sides disagreed on a single byte of key layout, a rule would silently never match. Keeping the packing in one crate makes that class of bug impossible.

### 3. The rule store and SIGHUP reconcile

The loader owns the authoritative rule store: a JSON file at `/var/lib/sluice/rules.json` (root-owned, mode `0600`). Each rule is a destination IP and an optional port (`0` = any port). On startup the loader reads the file and inserts every rule into the appropriate kernel map. On **SIGHUP** it re-reads the file from scratch — clearing the maps, then reloading — so an operator can edit the file directly and reconcile the kernel state without restarting the engine.

There are two trust levels for rules, on purpose:

- **Rules loaded from the file** are trusted as-is. The file is root-owned; if you can write it, you already have root.
- **Rules set over the gRPC link** (from the unprivileged UI) pass an engine-side **safelist** first. The engine refuses to block loopback addresses (`127.0.0.0/8`, `::1`) or the unspecified address (`0.0.0.0`, `::`), so even if the UI's own guard were bypassed, a request coming over the wire can never strand the box's local services or DNS stub.

Every rule change made over gRPC is applied to the live kernel maps **and** persisted back to the JSON file in the same operation, so rules survive an engine restart.

### 4. The gRPC server over a hardened Unix socket

The engine is the gRPC **server**; the UI dials in as the client. This is the inverse of the usual desktop pattern, and it's deliberate: the privileged side is the stable, long-lived process, and it pushes events to whatever UI happens to be attached.

The socket lives at `/run/sluice/engine.sock` and is hardened three ways:

- The containing directory and the socket file are created by root, and the socket is `chown`ed to the owner's UID and set to mode `0600` — so only the owner (and root) can open it.
- Every accepted connection is checked with **`SO_PEERCRED`**: the engine reads the connecting process's real UID from the kernel and rejects anyone who isn't the owner or root. This is defence in depth on top of the file permissions.
- The link is **local-only** — a Unix domain socket, never a TCP port. The engine makes no network egress of any kind.

The service contract (`crates/sluice-proto/proto/sluice.proto`) is small:

| RPC | Direction | Purpose |
|---|---|---|
| `WatchConnections` | server-streaming | the live feed — every observed connection, with its verdict |
| `SetRule` / `RemoveRule` / `ListRules` | unary | the two-click block/allow flow and the rules panel |
| `GetInboundPolicy` / `SetInboundPolicy` | unary | the inbound enforcement posture |

`ConnEvent` (the streamed message) carries the destination, port, protocol, verdict, the resolved hostname, the owning process's path / `comm` / command-line args, the container label, and an `inbound` flag. The engine fills these in before sending — which it can do reliably because it's root and can read any process's `/proc`; the unprivileged UI could not.

### 5. The ring-buffer drain and enrichment

A single async task drains the ring buffer. For each `ConnEvent`:

- It counts the connection and logs blocks to the journal (blocks are a security audit trail; allows are logged only with a debug flag, to keep the journal quiet).
- **Only if a UI is actually attached** does it do the expensive enrichment — reading the process's executable path, `comm`, and command line from `/proc/<pid>`, and resolving the container the process runs in. When no UI is watching, enrichment is skipped entirely, so the engine costs almost nothing in the background.
- It looks up the destination IP in the DNS cache to attach a hostname.
- It broadcasts the enriched event to all connected UI clients.

**Container attribution** (`engine/loader/src/container.rs`) maps a PID to its container by parsing `/proc/<pid>/cgroup`. Docker containers get their friendly name resolved from the container's on-disk metadata (`/var/lib/docker/containers/<id>/config.v2.json`) — no Docker socket, no extra dependency — and cached. Podman, containerd, CRI-O, and systemd-nspawn are recognised and fall back to a `runtime:shortid` label. This is what lets the feed group, say, all of a container's probes under one name.

### 6. DNS snoop

The eBPF hook only ever sees an IP address — the application resolved the name long before `connect(2)`. To label connections with the hostname the user would recognise, the engine runs a **passive DNS snoop** (`engine/loader/src/dns.rs`): an `AF_PACKET` socket that captures DNS *responses* (UDP source port 53) across all interfaces, parses the A and AAAA answers, and builds an in-memory `IP → hostname` cache (TTL-bounded, size-capped). The IPv4 transport carries both A and AAAA records, so IPv6 destinations get names too.

This is read-only and makes no network calls of its own — it only observes DNS traffic the machine was already sending. The cache is **in-memory only and never persisted**, because it's browsing PII. Encrypted DNS (DoH/DoT) is opaque to this snoop by design; those destinations simply show up as IPs.

### Inbound: observer + nftables enforcement

The `cgroup/connect` hook is outbound-only — it fires when *your* machine reaches out. Inbound (a remote peer reaching *you*) is handled by two separate, independent pieces.

**The observer** (`engine/loader/src/inbound.rs`) subscribes to the kernel's **conntrack `NEW`** events over a netlink socket. For each new flow where a non-local peer connected to one of the machine's own addresses, it synthesises an inbound `ConnEvent` and streams it to the UI — so incoming connections appear in the feed alongside outbound ones. This is strictly observe-only: it reads kernel events and changes nothing, so it cannot strand the box.

**Enforcement** (`engine/loader/src/nft.rs`) is opt-in and managed through nftables. When enabled, the engine programs a dedicated `inet sluice` table with a default-deny `input` chain that always accepts:

- `established` / `related` flows — so replies to your own outbound traffic survive,
- loopback,

and then the user's explicit allow-list of `(protocol, port)` entries, before dropping everything else. The ruleset is applied as a single atomic `nft` transaction. The table is removed on engine startup (clearing any stale table from a prior crash) and on exit (so stopping the engine reopens inbound). Inbound enforcement is **off by default**; the posture is persisted to `/var/lib/sluice/inbound.json` and toggled from the UI.

## The UI

`sluice-ui` (in `crates/sluice-ui`) is a Tauri 2 desktop app: a Rust backend with a static webview frontend. It is an unprivileged gRPC **client** of the engine. The frontend (`frontend/index.html` + `frontend/main.js`) is dependency-free — plain HTML, CSS, and JavaScript with **no build step** — so you can edit a page and reload.

### The feed pipeline

On startup the backend opens the engine's `WatchConnections` stream over the Unix socket and reconnects with backoff if the engine isn't up yet (or restarts). For each streamed `ConnEvent` it:

1. maps the engine message into the UI's internal feed model (`sluice-types`),
2. computes a display row — including a **project/launcher label** for grouping (the outermost non-system ancestor, or the container name when known), and a **first-seen** flag the first time a given binary ever reaches the network,
3. **batches** rows into one webview emit every ~150ms, so a machine making thousands of connections a second can't flood the UI thread and freeze the window,
4. persists resolved rows to the local history database.

The same pipeline fires desktop notifications for engine connect/disconnect transitions and for genuinely new applications coming online (throttled, with a startup warm-up so a fresh history doesn't fire dozens at once).

`crates/sluice-types` holds the shared, UI-agnostic value types — the connection model (`pb::Connection`), the feed event model (`FeedEvent`, `ConnState`), and the verdict enums (`Action`, `Scope`, `RuleDuration`). The gRPC wire types live separately in `crates/sluice-proto`; `sluice-types` has no gRPC dependency, which keeps the feed/verdict logic easy to test in isolation.

### Writing rules

The two-click block/allow flow maps a user's choice onto the engine's destination-keyed rule model:

- **Block** a destination → `SetRule(ip, port)` (or port `0` for "any port to this host"), which the engine inserts into the kernel blocklist and persists.
- **Allow** a destination → since the engine is default-allow, "allow" means *removing* any matching block rule (a no-op if there wasn't one).

Before sending any block, the UI applies its own **protected-host** guard: it refuses to block `localhost`, `*.localhost`, any loopback IP, or an empty host — matched exactly (not by substring), so an attacker-named host like `localhost.evil.com` is still blockable and a real address that merely contains `::1` is not wrongly protected. This is belt-and-braces with the engine's own safelist: the same protection exists on both sides of the boundary. Removing a block rule (which *reduces* protection) is gated by a confirmation dialog.

### The views

The app is a single window with a left rail switching between in-window views (not modals):

- **Feed** — the live decision stream. Coalesced by count, with free-text search and filters (protocol, category, local-only, verdict, and a time window of live / hour / today / 7d). Each row expands to show PID/UID, source and destination addresses, protocol, the verdict and why, the process tree and command-line args, the destination country (offline geo-IP lookup), and on-demand reverse-DNS and an external "look up host" link. Connections are grouped by launcher/container, with the most-recently-active group floated to the top.
- **Usage** — top talkers: the most active hosts or apps over a chosen window, with an allowed/blocked split, sortable columns, search, and per-row drill-down (a host → the apps that reached it; an app → its hosts), with block/allow available right there.
- **Apps** — every application that has touched the network, with its activity from history.
- **Destinations** — every destination reached, with real posture overlaid from the engine's deny rules (a destination is "blocked" when a deny rule covers one of its IPs).
- **Security** — a log of security-relevant events (engine alerts, new-app-online events), with severity, search, drill-down, and block/allow on actionable events.
- **Inbound** — the inbound policy view: a default-deny toggle plus the allow-list of open ports, and per-port drill-down into the remote peers that reached each port.
- **Settings** — history retention, bandwidth units, and related preferences.

A live **bandwidth graph** sits permanently on top, collapsible. Its data does *not* come from the engine: connection events are counts, not byte volume, so the UI runs a small **unprivileged collector** (`netstat.rs`) that samples `/proc/net/dev` ~1 Hz and diffs the cumulative byte counters to produce total host throughput in/out (loopback excluded). This is world-readable, purely local, and dependency-free.

The app also provides a **system tray** (Show / Quit; closing the window hides to tray and keeps running) and desktop notifications.

### Local history

The UI keeps its own SQLite database at `~/.local/share/sluice/history.db` (`rusqlite`, bundled), in a `0700` directory with the database file at `0600` — because connection history (hosts, paths, IPs) is PII and must be owner-only at rest. It stores resolved feed decisions and security events, so the feed and counts survive restarts. It is **entirely separate** from the engine's rule store: the two never share a file, wiping one never corrupts the other, and the history lives under the user's data dir, not a privileged location. If the data directory is somehow unwritable, the store degrades gracefully to in-memory rather than breaking the app. History is user-clearable and retention is configurable.

## End-to-end data flow

Putting it together, here is the life of one connection — say a browser reaching `example.com`:

1. The browser calls `connect(2)`. The kernel runs the `cgroup/connect4` eBPF program.
2. The program reads the PID/UID/destination/protocol, checks the blocklist maps, stamps the verdict, emits a `ConnEvent` to the ring buffer, and returns allow/deny to the kernel. `connect(2)` either proceeds or gets `EPERM`. (Elapsed: ~1µs.)
3. The engine's drain task picks the event off the ring buffer, enriches it with the process path / args / container and the hostname from the DNS snoop, and broadcasts it.
4. The event travels over the Unix socket to the UI's `WatchConnections` stream.
5. The UI maps it into a feed row, batches it, emits it to the webview (where it appears in the feed), and writes it to SQLite.
6. You click **Block** on that row. The UI checks its protected-host guard, then calls `SetRule` over the socket.
7. The engine validates against its safelist, inserts the rule into the kernel blocklist map, and persists it to `rules.json`. It acknowledges.
8. The browser's *next* connection to that destination hits the now-populated blocklist and is denied in-kernel.

The decision path (steps 1–2) is kernel-only and synchronous. The feed path (steps 3–5) is asynchronous and best-effort — a slow or absent UI never slows the machine down. Rule writes (steps 6–8) are the only time the UI talks back, and they go through validation on both sides of the boundary.

## Trust boundary and threat surface

The boundary is the Unix socket between the two processes. The threat model treats it seriously:

- **The engine is the trusted, minimal, privileged component.** Its attack surface is: the eBPF programs it loads, the netlink/AF_PACKET sockets it reads, the nftables table it manages, the `/proc` and Docker metadata it parses, and the single local gRPC socket it serves. It makes no network connection and serves no remote client.
- **The socket is owner-or-root only** — enforced by file permissions *and* a kernel-checked `SO_PEERCRED` UID check on every connection.
- **Rules from the unprivileged side are validated** against a safelist before they ever reach the kernel, so a compromised or buggy UI cannot ask the engine to strand the machine.
- **The engine is fail-safe.** Default-allow means an empty rule set blocks nothing, and the eBPF programs auto-detach on exit, so the engine can never be the reason you lose connectivity. The single recovery command (`sudo systemctl stop sluice-engine`) always restores an open network.
- **Sluice is quiet by default.** Neither half makes a network connection on its own. The only egress is user-initiated: clicking "look up host" (reverse DNS via the system resolver) or the external investigate link. The default feed stays silent in its own output.
- **PII is contained.** The DNS cache is in-memory only; the history database and the engine state directory are both `0700`/`0600` and owner-scoped; per-connection details are not logged at default verbosity.

## Key design decisions and tradeoffs

**eBPF in-kernel decisions vs. a userspace per-connection prompt.** The defining choice. Holding every new connection in userspace to ask for a verdict adds *seconds* of latency on a busy host — the packet path saturates, drops, and retransmits. Sluice instead makes the allow/deny decision in-kernel from a map lookup and pushes the event to userspace *asynchronously*. Enforcement is decoupled from the UI: the feed can lag, the UI can be closed, and the machine stays fast. The cost is that interactive "hold this connection and ask me" isn't on the synchronous path — the model is observe-and-act (see it in the feed, then block) rather than block-and-prompt.

**Default-allow + denylist, not default-deny.** The engine only denies what's explicitly listed. This is a safety choice: a firewall that defaults to deny can lock you out of your own machine through a single bad rule or a crashed control plane. Default-allow means the worst case is "a destination you wanted blocked wasn't" — never "I can't reach anything." The protective posture you build is additive and visible, and stopping the engine always reopens everything.

**Two processes with a hard privilege split.** The privileged engine is small and changes rarely; the feature-rich UI holds no privilege. A bug in the large, fast-moving half cannot reach the kernel. The cost is the gRPC boundary and keeping the proto contract in sync — a price worth paying for the blast-radius reduction.

**Connection counts, not byte volume.** The engine reports *connection events*, not bytes — attributing per-app byte volume would require a much deeper (and more invasive) kernel data path. The usage and per-app views are therefore in terms of connection counts, which answers "who is talking to whom, how often" cleanly. Total host throughput (the bandwidth graph) is filled in by a separate, unprivileged `/proc/net/dev` sampler — accurate for the machine as a whole, just not split per app.

**A separate engine workspace, on a separate toolchain.** The `engine/` directory is intentionally **outside** the main `crates/` workspace, because the eBPF crate builds for a different target (`bpfel-unknown-none`) on **nightly** Rust with `rust-src` and `bpf-linker`, while the rest of Sluice is pinned to **stable 1.89**. Keeping them as detached workspaces lets each use the toolchain it needs without fighting a shared `rust-toolchain.toml`. The shared ABI crate (`engine/common`) is the bridge.

**Inbound as two independent pieces.** Inbound observation (conntrack) and inbound enforcement (nftables) are deliberately separate from the outbound eBPF path and from each other. Observation is always safe and on; enforcement is opt-in, atomic, and self-healing on restart. This keeps the always-on path free of anything that could strand the box, and lets inbound filtering be a posture you turn on consciously.

## Build and install

The repository has two build domains:

- **The main workspace** (`crates/*`) — the UI, the proto crate, and the shared types — builds on **stable Rust 1.89**. It needs `protoc` (for the gRPC codegen) and the WebKitGTK / Tauri system libraries.
- **The engine** (`engine/*`) — the eBPF program needs **nightly** Rust with the `rust-src` component and `bpf-linker`; the loader builds on stable.

A one-command installer (`install.sh`) builds both halves, installs the engine as a systemd service (`sluice-engine`, with its runtime and state directories), and installs the UI as a `.deb` with an app-menu entry. `uninstall.sh` reverses it. The current version is in `VERSION`.

## What isn't here

Listing the deliberate omissions is as useful as listing the parts:

- **No network egress from the firewall itself.** Neither the engine nor the UI phones home. Reputation lookups, update checks, and the like are not built in; the only outbound traffic is user-initiated host lookups.
- **No frontend framework or build step.** The webview is plain HTML/CSS/JS served as-is.
- **No per-app byte accounting.** The engine reports connection counts; total throughput comes from a separate sampler. Per-app byte volume is out of scope for the current data path.
- **No interactive connection-hold prompt on the hot path.** By design — the model is observe-and-act, to keep the kernel path non-blocking and the machine fast.
- **No shared state between the two processes except the socket.** The engine's rule store and the UI's history database are independent files in independent locations.

## Further reading

- `README.md` — what Sluice is and how to get started.
- `INSTALL.md` — installation and host prerequisites in detail.
- `DEVELOPMENT.md` — building and running Sluice locally for development.
- `SECURITY.md` — the threat model and security posture.
- `CONTRIBUTING.md` — workflow and conventions.
- `CHANGELOG.md` — release history.
