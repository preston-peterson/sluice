# Development setup

How to build, run, and work on Sluice locally — including the slightly unusual
two-workspace layout, the toolchain split between stable and nightly, and the dev
gotchas worth knowing before you spend an afternoon on them.

> Heads-up: Sluice is a firewall. The engine enforces connection policy in the
> kernel. While developing you can keep it entirely in observe/monitor mode (the
> default) so it never holds or drops traffic — see [Running safely](#running-safely).
> Recovery, if anything ever feels wrong, is always one command:
> `sudo systemctl stop sluice-engine`.

## The short version

```bash
git clone <repo-url> sluice
cd sluice

# 1. Install the build toolchain + system libraries (idempotent).
just setup

# 2. Build + verify the unprivileged half (the desktop app workspace).
just check

# 3. Build the engine (eBPF object + root loader) and install it as a service.
just engine-build
sudo engine/install.sh
sudo systemctl enable --now sluice-engine

# 4. Run the desktop app against the running engine.
just ui
```

`just setup` and `just check` cover everything you need to hack on the desktop UI.
You only need steps 3–4 when you want to exercise the full system end-to-end against
a real, enforcing engine.

If you don't have [`just`](https://github.com/casey/just), every recipe is a thin
wrapper over plain `cargo`/`sh` and the underlying commands work standalone — read
the `justfile` and run the body directly.

## The two halves (and why two workspaces)

Sluice is split by privilege, and the split shows up in the source tree as **two
separate Cargo workspaces with two different toolchains**:

| | Root workspace (`crates/*`) | Engine workspace (`engine/*`) |
|---|---|---|
| **Runs as** | your user, unprivileged | root (systemd service) |
| **What it is** | the Tauri desktop app + its support crates | the in-kernel enforcement engine: eBPF program + root loader |
| **Toolchain** | stable, pinned to **1.89** (`rust-toolchain.toml`) | **nightly** for the eBPF crate (`engine/ebpf/rust-toolchain.toml`) |
| **Build target** | host (your machine's native target) | the eBPF program builds for `bpfel-unknown-none`; the loader builds for the host |
| **gRPC role** | client | server |

They talk to each other over a hardened Unix-domain socket using a small gRPC
contract (`crates/sluice-proto/proto/sluice.proto`). The engine is the **server**;
the desktop app is the **client**. The engine streams every observed connection and
answers decision RPCs (add/remove rules, set the inbound policy); the app renders the
feed and writes rules. All privileged work happens in the engine — the desktop app
never needs root.

**Why the engine is a detached workspace.** The eBPF program is compiled for a
bare-metal, `no_std` target (`bpfel-unknown-none`) and needs nightly Rust plus
`rust-src` so it can build its own `core` via `-Z build-std`. That is a completely
different build world from the host-target desktop app, which is pinned to a specific
stable release for reproducibility. Keeping `engine/*` out of the root workspace lets
each side pin its own toolchain without fighting: the root tree stays on stable 1.89,
and the eBPF crate quietly uses nightly via its own `rust-toolchain.toml`. The shared
gRPC stub crate (`crates/sluice-proto`) is a member of the root workspace but uses
explicit version pins instead of workspace inheritance, precisely so the detached
engine loader can depend on it cross-workspace by path.

## Prerequisites

`just setup` (which runs `scripts/setup.sh`) installs and verifies all of these. It's
idempotent — safe to run repeatedly. If you'd rather install by hand, here's the
full list:

### For the desktop app (root workspace)

- **Rust, stable 1.89** — pinned in `rust-toolchain.toml`. With `rustup`, the correct
  channel is provisioned automatically the first time you run `cargo` inside the repo.
- **`protoc`** (the Protocol Buffers compiler) — required at build time to generate the
  gRPC stubs in `sluice-proto`. On Debian/Ubuntu: `protobuf-compiler`.
- **A C toolchain + `pkg-config`** — for the `-sys` crates. On Debian/Ubuntu:
  `build-essential pkg-config`.
- **WebKitGTK and the Tauri system libraries** — the desktop app uses a system webview.
  On Ubuntu these are:

  ```
  libwebkit2gtk-4.1-dev libsoup-3.0-dev librsvg2-dev \
  libxdo-dev libayatana-appindicator3-dev libssl-dev
  ```

- **The Tauri CLI** (`cargo install tauri-cli`) — only needed to build an installable
  `.deb` via `just package`; not needed for plain `cargo run`/`cargo test`.

### Additionally, for the engine

- **Rust nightly + the `rust-src` component** — for the eBPF crate's `-Z build-std`.

  ```bash
  rustup toolchain install nightly
  rustup component add rust-src --toolchain nightly
  ```

- **`bpf-linker`** on `PATH`:

  ```bash
  cargo install bpf-linker --locked
  ```

  Install it with the stable toolchain (`cargo +stable install bpf-linker --locked`)
  if the repo's pin gives you trouble — `bpf-linker` itself wants a recent compiler and
  doesn't need to honor the repo pin.

- **A cgroup-v2 unified hierarchy** at `/sys/fs/cgroup` (check with
  `stat -fc %T /sys/fs/cgroup` → should print `cgroup2fs`). This is the default on
  modern Ubuntu.
- **Kernel BTF** at `/sys/kernel/btf/vmlinux` (default on modern kernels) — needed for
  the eBPF program to load against your running kernel.
- **`nftables`** — the engine uses an `nft` table for inbound enforcement.

The desktop app builds on any machine with the first group installed; the engine needs
the kernel features in the second group and is built/run on the Linux box you're
actually firewalling.

## Repository layout

```
sluice/
├── Cargo.toml                  # root workspace (stable 1.89): the desktop app + support crates
├── rust-toolchain.toml         # pins stable 1.89 for the root workspace
├── justfile                    # common dev tasks (setup / check / ui / engine-build / …)
├── install.sh                  # one-command from-source install (builds the combined .deb: engine + UI)
├── uninstall.sh                # reverse install.sh
├── VERSION                     # current version (0.1.8)
├── scripts/
│   ├── setup.sh                # idempotent dev bootstrap (toolchains, protoc, Tauri libs)
│   └── fetch-geoip.sh          # fetch the offline IP-to-country database (per-machine, not committed)
│
├── crates/                     # ── ROOT WORKSPACE (stable 1.89) ──────────────────────
│   ├── sluice-ui/              # the Tauri desktop app — the unprivileged gRPC client
│   │   ├── src/                #   main.rs (commands + event plumbing), history.rs (SQLite),
│   │   │                       #   geoip.rs (offline country lookup), netstat.rs (throughput)
│   │   ├── frontend/           #   the static webview: index.html + main.js + style.css (no JS build)
│   │   ├── icons/              #   multi-size app/tray icons
│   │   ├── capabilities/       #   Tauri capability/permission manifest
│   │   └── tauri.conf.json     #   window, bundle, and CSP config
│   ├── sluice-proto/           # generated gRPC stubs from proto/sluice.proto (engine↔UI contract)
│   │   ├── proto/sluice.proto  #   the wire contract (the source of truth)
│   │   └── build.rs            #   tonic-build codegen (needs protoc)
│   └── sluice-types/           # plain shared value types (connection/feed/decision); no deps
│
└── engine/                     # ── DETACHED ENGINE WORKSPACES (nightly eBPF) ──────────
    ├── README.md               # engine-specific build/run/test notes
    ├── common/                 # #[repr(C)] kernel↔userspace ABI (no_std), shared by ebpf + loader
    ├── ebpf/                   # the eBPF program: cgroup/connect4+6, ring buffer, blocklist maps
    │   └── rust-toolchain.toml #   pins NIGHTLY for this crate
    ├── loader/                 # the root daemon (sluice-engine): loads/attaches the program,
    │   └── src/                #   reconciles rules, drains events, serves gRPC; also builds the
    │                           #   sluice-rule + sluice-watch no-GUI helper clients
    ├── install.sh              # install the engine as a root systemd service
    ├── uninstall.sh            # remove the engine service
    └── *.sh / *.py             # per-phase build/run/measure/test scripts
```

### What each crate does

- **`sluice-ui`** — the desktop app. A Tauri shell (Rust core) plus a dependency-free
  static web frontend. It connects to the engine as a gRPC client over the Unix socket,
  renders the live connection feed, and turns Allow/Block clicks into decision RPCs.
  It keeps its own SQLite history at `~/.local/share/sluice` (so the feed survives
  restarts), samples total host throughput from `/proc/net/dev` for the bandwidth graph,
  and does offline country lookups from a local database. It is **entirely
  unprivileged** — every privileged action is delegated to the engine.
- **`sluice-proto`** — the gRPC contract. `build.rs` compiles `proto/sluice.proto` into
  Rust server + client stubs at build time (this is why `protoc` is a hard prerequisite).
  The generated code is shared by both the desktop app and the engine loader.
- **`sluice-types`** — small, dependency-free value types (the connection / feed /
  decision shapes the app maps engine events into). Kept separate from the wire types so
  the UI layer isn't coupled to the gRPC representation.
- **`engine/common`** — the `#[repr(C)]`, `no_std` ABI shared across the kernel/userspace
  boundary: the connection-event record the eBPF program emits and the packed rule keys
  it looks up. Compiled into both the eBPF program and the loader so both sides agree on
  the byte layout.
- **`engine/ebpf`** — the eBPF program itself: `cgroup/connect4` and `connect6` hooks
  that consult the in-kernel blocklist maps (`BLOCKLIST` for IPv4, `BLOCKLIST6` for
  IPv6), deny matches, and emit a record for every connection over a ring buffer. Builds
  for `bpfel-unknown-none` on nightly.
- **`engine/loader`** — the root daemon, `sluice-engine`. It loads the eBPF object,
  attaches the hooks to the cgroup-v2 root, drives the blocklist maps, async-drains the
  ring buffer, enriches each event with process info from `/proc` (and Docker container
  labels where applicable), labels destinations with hostnames from a passive DNS snoop,
  manages the `nft inet sluice` table for inbound policy, persists rules to
  `/var/lib/sluice/rules.json` (reconciled on `SIGHUP`), and serves the gRPC stream and
  decision RPCs over the hardened socket. The same crate also builds two no-GUI helper
  clients: `sluice-rule` (add/remove/list rules from the command line) and `sluice-watch`
  (tail the live event stream).

## The justfile

Run `just` with no arguments to list everything. The recipes you'll use most:

| Recipe | What it does |
|---|---|
| `just setup` | Install the dev toolchain + system libs and verify prerequisites (runs `scripts/setup.sh`). Idempotent. |
| `just check` | The CI gate: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` over the root workspace. |
| `just fmt` | Auto-format the tree (`cargo fmt --all`). |
| `just ui` | Run the desktop app (`cargo run -p sluice-ui`). Connects to the engine over the Unix socket. |
| `just engine-build` | Build the engine: the eBPF object (nightly) **and** the host loader (`engine/ebpf` then `engine/loader`, both release). |
| `just engine-install` | `engine-build` + `sudo engine/install.sh` — install the engine as a root systemd service. |
| `just package` | Build an installable `.deb` (release build + the Tauri bundler). Output under `target/release/bundle/deb/`. Needs the Tauri CLI. |
| `just audit` | Supply-chain scan of dependencies against the security advisory database (`cargo audit`). Kept separate from `check`. |
| `just geoip` | Fetch the offline IP-to-country database for destination-country lookups. Per-machine; not committed; read locally at runtime with no network calls. |

## Building each part

### The desktop app

```bash
# build + test + lint everything in the root workspace
just check

# or just build the app
cargo build -p sluice-ui
```

This is pure stable Rust — no nightly, no eBPF, no root. It does need `protoc` (to
generate the gRPC stubs) and the WebKitGTK/Tauri system libraries (to build and run the
webview).

### The engine

```bash
just engine-build
# equivalent to:
( cd engine/ebpf   && cargo build --release )   # nightly + rust-src, target bpfel-unknown-none
( cd engine/loader && cargo build --release )   # host target, stable is fine
```

The eBPF crate's own `rust-toolchain.toml` selects nightly automatically; you don't pass
`+nightly` yourself. The loader links against the compiled eBPF object at runtime (its
path is baked into the systemd unit, overridable with `SLUICE_BPF_OBJ`).

The eBPF program is checked by the **kernel verifier at load time**, not at compile time
— see [Gotchas](#dev-gotchas). A clean `cargo build` does not guarantee the program will
load on your kernel; you only find out when the loader attaches it.

## Running it

### Run the engine, then the app

The engine is the gRPC server; the app is its client. Bring up the engine first.

For day-to-day development the simplest path is to install the engine as a service:

```bash
just engine-build
sudo engine/install.sh
sudo systemctl enable --now sluice-engine
journalctl -u sluice-engine -f      # follow its log
```

`engine/install.sh` copies the loader + eBPF object to `/usr/lib/sluice`, writes a
systemd unit, and bakes in your uid as the permitted UI owner. It also warns if another
connection firewall is enabled at boot (two services both hooking `cgroup/connect` will
fight — disable the other one).

Then run the app, which connects to the engine over the default socket:

```bash
just ui
```

If you'd rather not install a service while iterating on engine code, you can run the
loader straight from the build tree (it needs root to load/attach the eBPF program), and
point both halves at a dev socket:

```bash
# terminal 1 — the engine (root). It auto-detaches all eBPF programs when you Ctrl-C it.
sudo SLUICE_ENGINE_UDS=/run/sluice/engine.sock SLUICE_OWNER_UID=$(id -u) \
    ./engine/loader/target/release/sluice-engine

# terminal 2 — the app (your user)
SLUICE_ENGINE_UDS=/run/sluice/engine.sock cargo run -p sluice-ui
```

The socket is local-only and reachable solely by the owner uid: the engine `chown`s it
`0600` in a `0700` directory and verifies the connecting peer's uid via `SO_PEERCRED`
(owner uid or root only). The app's SQLite history lives at `~/.local/share/sluice`
(also `0600` in a `0700` directory).

### Environment variables

The engine loader reads:

| Variable | Default | Purpose |
|---|---|---|
| `SLUICE_RULES` | `/var/lib/sluice/rules.json` | the JSON rule store; loaded at startup and on `SIGHUP`, rewritten by the decision RPCs |
| `SLUICE_ENGINE_UDS` | `/run/sluice/engine.sock` | the Unix socket the gRPC server listens on |
| `SLUICE_OWNER_UID` | falls back to `SUDO_UID` | the uid allowed to connect; without it the UI link is disabled (the engine still drains events to stdout) |
| `SLUICE_BPF_OBJ` | (built-in path) | path to the compiled eBPF object |
| `SLUICE_INBOUND` | (see source) | path to the inbound-policy store |
| `SLUICE_LOG_CONNS` | unset | set to also log allowed connections (verbose; debugging only) |

The desktop app reads `SLUICE_ENGINE_UDS` to find the engine, and the standard XDG
variables (`XDG_DATA_HOME`, `HOME`) to locate its history database and the offline geo
database.

### No-GUI clients

The engine ships two small command-line clients (built alongside the loader) that are
handy when developing without the full app running:

```bash
# tail the live connection stream
SLUICE_ENGINE_UDS=/run/sluice/engine.sock ./engine/loader/target/release/sluice-watch

# add / remove / list rules
./engine/loader/target/release/sluice-rule list
./engine/loader/target/release/sluice-rule block <ip> <port>
./engine/loader/target/release/sluice-rule remove <id>
```

## Running safely

This is firewall code; treat the live machine with respect.

- **The engine is default-allow.** An empty rule store blocks nothing — only listed
  destinations are denied. There is no posture that locks you out of the network by
  default.
- **The default UI posture is monitor.** It observes and shows connections without
  holding or dropping anything. Allow/Block are explicit, two-click actions.
- **The engine enforces an internal safelist** on rules written via the RPCs: it will
  never block loopback, the DNS stub resolver, or the unspecified address, so the box
  can't be stranded even if a UI guard is bypassed. (Rules edited directly into the
  root-owned store file are trusted as-is.)
- **eBPF programs auto-detach when the loader exits.** If you run the loader in the
  foreground, `Ctrl-C` removes the hooks and traffic flows freely again.
- **Recovery is always one command:** `sudo systemctl stop sluice-engine` (or stop the
  foreground loader). Enforcement ends immediately and the network is fully open.

Outbound is enforced for both IPv4 and IPv6 via the eBPF hooks; inbound is enforced via
the `nft inet sluice` table (default-deny input plus an allow-list, with
established/related and loopback always accepted so return traffic and local services
keep working).

## Testing

The root-workspace test suite runs with plain `cargo`:

```bash
cargo test --workspace
```

The engine has a set of **self-contained, root-run integration scripts** under
`engine/` that exercise the real kernel path end-to-end without touching external
networks (they block loopback ports, or use a network namespace as a simulated remote
peer). These need root because they load/attach eBPF and edit `nftables`:

```bash
sudo ./engine/measure-e0.sh    # latency: connect(2) cost with the hook attached vs not
sudo ./engine/test-e1.sh       # in-kernel deny: block a loopback ip:port, prove deny + reload
sudo ./engine/test-e3.1.sh     # decision RPCs: block/allow/list over gRPC, persists across restart
sudo ./engine/test-e3.sh       # the gRPC/UDS link: events stream to an unprivileged client
sudo ./engine/test-e4.sh       # destination-hostname + process/container enrichment
sudo ./engine/test-e6.sh       # inbound enforcement via the nftables input chain
```

These are blunt instruments meant to be read as much as run — each one is a short shell
script that sets up a precise condition (an exact `ip:port`, an exact port) so it can't
collide with anything else on the box. Read the script before running it.

## The CI gate

Every change must pass `just check` before it can merge. That's three steps, and CI runs
exactly the same ones:

1. **Formatting** — `cargo fmt --all -- --check`. Run `just fmt` to fix.
2. **Lints** — `cargo clippy --workspace --all-targets -- -D warnings`. **Warnings are
   errors**; a single Clippy warning fails the build. Fix them rather than `#[allow]`-ing
   unless there's a real reason.
3. **Tests** — `cargo test --workspace`.

Run `just check` locally before opening a PR — it's the cheapest way to avoid a red CI
run. `just audit` (the dependency advisory scan) is run separately and is worth running
after any dependency change.

## Versioning

The current version lives in `VERSION` (and is mirrored in the app's `tauri.conf.json`).
Bump it in its own commit when cutting a release; keep `CHANGELOG.md` in step.

## Dev gotchas

A few things that will eat an afternoon if you don't know them up front.

- **The eBPF program is verified by the kernel at load time, not at compile time.**
  `cargo build` succeeding only means the program type-checks and links. Whether it
  actually loads depends on the kernel verifier on *your* machine — it rejects programs
  that (in its analysis) might loop unboundedly, read out of bounds, or exceed
  complexity limits. If the loader fails to attach, the verifier log in
  `journalctl -u sluice-engine` (or the loader's stderr) is where the real error is. A
  change that looks innocent in Rust can be rejected by the verifier; iterate against an
  actual load, not just a build.
- **The system tray needs a GNOME extension.** GNOME doesn't show legacy tray icons out
  of the box. The Sluice tray icon (and its menu) only appears with the **"AppIndicator
  and KStatusNotifierItem Support"** GNOME extension installed and enabled. Without it the
  app still runs fine — you just won't see a tray icon. If the tray menu shows up but its
  labels render blank, that's the same extension intermittently dropping menu labels
  (typically after a GNOME Shell reload, unlock, or relogin), not a Sluice bug — toggle
  the extension off and on, or relaunch the app. (Sluice also re-publishes its tray menu
  shortly after startup and on window focus to nudge the labels back.)
- **WebKitGTK renders native form controls white-on-white in dark mode.** A `<select>`,
  checkbox, or other native control will be invisible against a dark background unless you
  explicitly set `appearance: none` plus a `background-color` (and style `<option>`
  too). If a control "disappears" in the dark theme, this is almost always why. Keep an
  eye on it whenever you add a new form control to the frontend.
- **Only one connection firewall can own the cgroup hooks.** If another
  `cgroup/connect`-based firewall is enabled at boot, it and the Sluice engine will fight
  over enforcement. `engine/install.sh` warns about known conflicts; disable the other
  service so only Sluice manages connections.
- **`protoc` must be on `PATH` for any build that touches the proto crate.** A missing
  `protoc` fails the `sluice-proto` build script with a codegen error rather than an
  obvious "install protoc" message. `just setup` installs it; if you build in a fresh
  environment, install it first.
- **The two workspaces don't share a lockfile or a toolchain.** Running `cargo` from the
  repo root operates on the root workspace (stable). The engine crates each have their
  own `Cargo.lock` and are built from inside `engine/ebpf` and `engine/loader`. A
  workspace-wide `cargo test` does **not** build or test the engine — use
  `just engine-build` and the `engine/*.sh` scripts for that.

## Where to ask

Open an issue with the `question` label. If you're adding something substantial — a new
view, an engine feature, anything that crosses the privilege boundary — it's worth
sketching the approach in an issue first so the design can be discussed before the code
is written.
