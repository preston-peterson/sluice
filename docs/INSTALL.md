# Installation & Operations Manual

This is the long-form guide for installing, running, updating, and removing
Sluice. The README has the ~3-minute version; this file has everything else —
what the installer does step by step, every flag, how to manage the two halves
once they're running, and a troubleshooting section.

If you just want to get it running, jump to [Installation](#installation). If
you've already installed and need something specific, the table of contents
below has direct links.

## Table of contents

- [What gets installed](#what-gets-installed)
- [Requirements](#requirements)
- [Installation](#installation)
  - [One-command install (recommended)](#one-command-install-recommended)
  - [Installer flags](#installer-flags)
  - [What the installer does, step by step](#what-the-installer-does-step-by-step)
  - [Manual / split install](#manual--split-install)
- [First run](#first-run)
  - [Launching the app](#launching-the-app)
  - [The system tray (GNOME)](#the-system-tray-gnome)
- [Where things live](#where-things-live)
- [Managing the engine service](#managing-the-engine-service)
- [Recovery — opening the network back up](#recovery--opening-the-network-back-up)
- [Updating](#updating)
- [Uninstalling](#uninstalling)
- [Building from source (developers)](#building-from-source-developers)
- [Troubleshooting](#troubleshooting)

## What gets installed

Sluice is **two parts with a clean privilege split**, and the installer sets up
both:

1. **The engine** — a small **root systemd service** (`sluice-engine`) that does
   the actual enforcement. Outbound control is an in-kernel eBPF
   `cgroup/connect` program that denies connections from kernel rule maps (IPv4
   and IPv6) and streams every connection event to the UI; inbound control is an
   `nftables` table plus a connection-tracking observer. A passive DNS snoop
   labels addresses with hostnames, and `/proc` plus container enrichment add
   process and container context. The engine persists rules to disk, reloads
   them on demand, and serves the UI over a hardened Unix socket.

2. **The desktop UI** — an **unprivileged** desktop app (the binary is
   `sluice-ui`, shown as **"Sluice"** in your app menu). It is a *client* of the
   engine: it renders the live feed and writes rules, but the root engine does
   every privileged operation. The UI never runs as root and makes no network
   calls of its own. It keeps its own local history in your home directory.

The installer builds both from source, installs the engine as a root service,
and installs the UI as a system `.deb` package.

> **Safe by default.** The engine is **default-allow with a denylist**, so a
> running engine never locks you out of the network — only the destinations you
> explicitly block are blocked. The UI's default posture is **monitor**, which
> watches without ever holding traffic. If anything ever goes wrong, recovery is
> always a single command: `sudo systemctl stop sluice-engine`.

## Requirements

- **Ubuntu (GNOME)** — the supported target is Ubuntu 24.04 LTS on GNOME. Other
  recent systemd-based distributions with a modern kernel will very likely work
  but aren't a release gate.
- **A Linux kernel with eBPF + cgroup v2** — standard on Ubuntu 24.04. The
  engine attaches an eBPF `cgroup/connect` program; this is built into the
  default kernel.
- **`sudo` access.** The installer runs as your normal user and calls `sudo`
  only for the steps that genuinely need root (installing system packages, the
  systemd service, and the `.deb`). **Do not run the installer as root** — it
  will refuse, because the UI is per-user and the engine bakes in the invoking
  user's UID as the authorized owner of the control socket.
- **Internet access at install time**, to pull the build toolchain and system
  libraries. At *runtime* Sluice makes no outbound calls of its own.
- **Build prerequisites** (the installer provisions these for you on Ubuntu —
  see the step-by-step below): a Rust toolchain (stable, plus a nightly with
  `rust-src` and `bpf-linker` for the eBPF object), `protoc` (the protobuf
  compiler, for the gRPC contract), and the desktop build libraries (WebKitGTK
  and friends).

For the GNOME system tray to show the Sluice tray icon, you'll also want the
**"AppIndicator and KStatusNotifierItem Support"** GNOME Shell extension — see
[The system tray](#the-system-tray-gnome). The app runs fine without it; only
the tray icon is affected.

## Installation

### One-command install (recommended)

From a checkout of the repository, run:

```bash
./install.sh
```

That's it. Run it **as your normal user** — it will `sudo` only where it needs
to. The script builds both halves from source, installs and starts the root
engine service, and builds and installs the desktop UI as a `.deb`. On a fresh
machine it also provisions the build toolchain (a first run that compiles the
eBPF object, the engine, and the Tauri-bundled UI takes a while — subsequent
runs are faster).

When it finishes you'll see a summary like:

```
Sluice installed.
  • Launch the UI from your app menu ("Sluice") or run: sluice-ui
  • Engine service: systemctl status sluice-engine
  • Recovery (open the network back up): sudo systemctl stop sluice-engine
  • Uninstall: ./uninstall.sh
```

### Installer flags

```
./install.sh              Full install: prerequisites + engine + UI
./install.sh --skip-deps  Skip the prerequisite/toolchain step (already set up)
./install.sh --engine     Install or refresh ONLY the engine (skip the UI)
./install.sh --ui         Build and install ONLY the desktop UI (.deb)
./install.sh --help       Show usage and exit
```

- `--skip-deps` skips the toolchain provisioning step. Use it when you've
  already run a full install once, or you manage the Rust/`protoc`/WebKitGTK
  prerequisites yourself, and just want to rebuild and reinstall.
- `--engine` rebuilds the eBPF object and the engine loader and (re)installs the
  root service, leaving the desktop UI untouched. Handy after pulling engine
  changes.
- `--ui` builds the `.deb` and installs only the desktop app. It implies
  `--skip-deps` (the UI build doesn't need the eBPF toolchain), so the engine
  service is left exactly as it is.

### What the installer does, step by step

`./install.sh` runs four phases. Steps 2–3 are skipped with `--ui`; step 4 is
skipped with `--engine`; step 1 is skipped with `--skip-deps` (and with `--ui`).

1. **Prerequisites — the build toolchain.** Runs the repo's `scripts/setup.sh`,
   which (on Ubuntu, via `apt`) installs the protobuf compiler, `pkg-config`,
   `build-essential`, and the desktop build libraries (WebKitGTK, libsoup,
   librsvg, the AppIndicator dev library, and OpenSSL dev). It also ensures Rust
   is present via `rustup`. The installer then provisions the **eBPF toolchain**:
   a **nightly** Rust toolchain with the **`rust-src`** component, plus
   **`bpf-linker`** (installed with `cargo install bpf-linker` if it's missing —
   this compile takes a few minutes the first time).

2. **Build the engine.** Compiles the eBPF object
   (`engine/ebpf`, pinned to nightly by its own toolchain file) and the root
   loader (`engine/loader`) in release mode.

3. **Install the engine service (root).** With `sudo`, copies the loader and the
   eBPF object into `/usr/lib/sluice`, writes a systemd unit at
   `/etc/systemd/system/sluice-engine.service`, and bakes in your UID as the
   authorized owner of the control socket. It then enables and starts the
   service (`systemctl enable --now sluice-engine`) and reports whether it came
   up. The unit sets the runtime directory (`/run/sluice`), the state directory
   (`/var/lib/sluice`), the socket path, the rules-file path, and `Restart=always`
   so the engine comes back after a crash or reboot.

4. **Build and install the desktop UI (`.deb`).** Ensures the Tauri CLI is
   present (`cargo install tauri-cli` if needed), builds a `.deb` with
   `cargo tauri build --bundles deb`, and installs it with `apt`/`dpkg`. The
   package installs the `sluice-ui` binary, a **"Sluice"** application-menu
   entry, and the icons. The `.deb` is left in the build tree at
   **`target/release/bundle/deb/`** if you want to copy it to another machine.

> **One firewall at a time.** The engine hooks the kernel's `cgroup/connect`
> path. If you have another always-on connection firewall enabled at boot,
> running two will fight over the same hook — the engine installer warns you and
> prints the command to disable the conflicting service so Sluice is the only one
> managing connections.

### Manual / split install

You don't have to use the top-level `install.sh`. The two halves can be built
and installed independently, which is the typical developer workflow.

**Engine only:**

```bash
# Build the eBPF object (nightly) and the loader (stable):
( cd engine/ebpf && cargo build --release )
( cd engine/loader && cargo build --release )

# Install + start the root service (bakes in your UID as the socket owner):
sudo engine/install.sh
sudo systemctl enable --now sluice-engine
```

(`just engine-build` and `just engine-install` wrap exactly these commands.)

**UI only:**

```bash
# Build the installable .deb:
( cd crates/sluice-ui && cargo tauri build --bundles deb )

# Install it:
sudo apt-get install -y target/release/bundle/deb/*.deb
```

(`just package` wraps the build step.) Or, for development, run the UI straight
from source against a running engine with `just ui` (i.e. `cargo run -p
sluice-ui`) — no `.deb` needed.

## First run

### Launching the app

Open **"Sluice"** from your application menu, or run `sluice-ui` from a
terminal. The app connects to the running engine over its Unix socket
automatically; if the engine isn't up yet, the app shows an engine-offline
state and reconnects when the service starts.

A first-run onboarding checklist walks you through the basics. The app starts in
**monitor** mode — it shows you everything that's connecting without ever
holding traffic — so it's safe to leave running while you get a feel for what's
on your machine. You can switch posture and start blocking from the UI whenever
you're ready; the engine enforces the blocks in-kernel.

Closing the window **hides Sluice to the tray** and keeps it running so the feed
and history keep flowing. Quit fully from the tray menu.

### The system tray (GNOME)

Vanilla GNOME hides legacy/AppIndicator tray icons, so out of the box you may
not see the Sluice tray icon. To get it back, install and enable the
**"AppIndicator and KStatusNotifierItem Support"** GNOME Shell extension (search
for it in GNOME Extensions / "Extension Manager"), then either relaunch Sluice
or toggle the extension off and on.

The app itself runs whether or not the extension is present — only the tray icon
depends on it. If you ever close the window and can't find the tray icon, just
launch **Sluice** again from the app menu.

## Where things live

| Path | Owner | What |
|---|---|---|
| `/usr/lib/sluice/sluice-engine` | root | The engine loader binary. |
| `/usr/lib/sluice/sluice-ebpf` | root | The compiled eBPF object the loader attaches. |
| `/etc/systemd/system/sluice-engine.service` | root | The engine's systemd unit. |
| `/run/sluice/engine.sock` | root | The hardened control socket (dir `0700`, socket `0600`, peer-credential gated to your UID or root). |
| `/var/lib/sluice/rules.json` | root | The persisted rule store (your blocks/allows). |
| `/usr/bin/sluice-ui` | system | The desktop app binary (installed by the `.deb`). |
| `~/.local/share/sluice/` | you | The UI's local history database (SQLite; `0600` in a `0700` directory). |

The engine's rules and the UI's history are deliberately separate — the root
engine owns enforcement state under `/var/lib/sluice`, and the unprivileged UI
owns its own history under your home directory.

## Managing the engine service

The engine is a standard systemd service, so the usual commands apply:

```bash
sudo systemctl status sluice-engine     # Is it running?
sudo systemctl restart sluice-engine    # Restart it
sudo systemctl stop sluice-engine       # Stop it (opens the network fully — see Recovery)
sudo systemctl start sluice-engine      # Start it again
sudo systemctl enable sluice-engine     # Start at boot (the installer does this)
sudo systemctl disable sluice-engine    # Don't start at boot
sudo journalctl -u sluice-engine -f     # Follow live logs
sudo journalctl -u sluice-engine -n 50  # Last 50 log lines
```

The UI is a per-user desktop app, not a service — start it from the app menu or
`sluice-ui`, and quit it from its tray menu.

## Recovery — opening the network back up

If a rule you set is causing trouble, or you simply want all enforcement off,
stop the engine:

```bash
sudo systemctl stop sluice-engine
```

When the engine stops, outbound enforcement (the eBPF program) is detached and
**inbound traffic reopens automatically**, so the network returns to its normal
unfiltered state immediately. Your rules are preserved in
`/var/lib/sluice/rules.json` and take effect again the next time the engine
starts.

Because the engine is default-allow with a denylist, a *running* engine can't
lock you out either — only destinations you explicitly blocked are blocked.
Stopping the service is the universal escape hatch regardless.

## Updating

Sluice updates are a rebuild-and-reinstall from a newer checkout:

```bash
git pull
./install.sh
```

Re-running `./install.sh` is safe and idempotent. To save time you can target
just the half that changed:

```bash
./install.sh --engine   # rebuild + reinstall only the engine service
./install.sh --ui       # rebuild + reinstall only the desktop UI
```

Your data is preserved across updates: the engine's `rules.json` and the UI's
history database are not touched by a reinstall. After an engine update the
service is restarted automatically; restart the desktop app yourself to pick up
a new UI build.

## Uninstalling

To remove Sluice, run the uninstaller as your normal user:

```bash
./uninstall.sh
```

It removes the `sluice-engine` service and its installed files
(`/usr/lib/sluice`) and removes the `sluice-ui` package. Stopping the engine
reopens inbound traffic, so by the end the network is back to normal. By default
the uninstaller is interactive and **prompts before deleting your data** (the
engine rule store and the UI history).

Flags control the data prompt:

```bash
./uninstall.sh           # Remove engine + UI; prompt about your data
./uninstall.sh --purge   # Remove everything including data, no prompts
./uninstall.sh --keep    # Remove engine + UI, keep all data, no prompts
./uninstall.sh --help    # Show usage and exit
```

- **Always removed:** the `sluice-engine` service + `/usr/lib/sluice`, and the
  `sluice-ui` package.
- **Your data** (kept unless you choose otherwise): `/var/lib/sluice` (the rule
  store) and `~/.local/share/sluice` (the UI history).

The uninstaller does not remove system packages (the Rust toolchain, `protoc`,
WebKitGTK, etc.), since those may be used by other software.

## Building from source (developers)

If you'd rather drive the build directly, the repo ships a `justfile` with the
common tasks (the underlying commands are plain `cargo`/`sh` and work without
`just` installed):

```bash
just setup          # Install the toolchain: Rust + protoc + the desktop/WebKitGTK libs
just check          # fmt + clippy + tests — the CI gate
just engine-build   # Build the eBPF object (nightly) + the root loader
just engine-install # Build, then install the engine as a root systemd service
just ui             # Run the desktop app from source against a running engine
just package        # Build the installable .deb (output under target/release/bundle/deb/)
```

Layout notes that matter when building:

- There are **two Rust workspaces**. The root workspace (`crates/*` — the
  desktop UI, the gRPC contract, and the shared value types) builds on the
  **pinned stable** toolchain. The detached engine workspace (`engine/*` — the
  eBPF program and the root loader) needs **nightly + `rust-src` + `bpf-linker`**
  for the eBPF crate.
- **`protoc` is required** to build either side, since the engine↔UI gRPC stubs
  are generated from the protobuf contract.
- Building the desktop UI needs the **WebKitGTK** dev libraries; `just setup`
  installs them.

The current version is recorded in the repo's top-level `VERSION` file
(`0.1.5`).

## Troubleshooting

**The engine service isn't active after install.**
Check its logs:

```bash
sudo systemctl status sluice-engine
sudo journalctl -u sluice-engine -n 50
```

The most common causes are a missing/stale eBPF object (rebuild with
`./install.sh --engine`) or another always-on connection firewall already
holding the `cgroup/connect` hook — the engine installer warns about a conflict
and prints the command to disable the other service. Restart with
`sudo systemctl restart sluice-engine` once resolved.

**The UI says the engine is offline.**
The desktop app talks to the engine over `/run/sluice/engine.sock`. Confirm the
service is up (`systemctl status sluice-engine`); if it is, make sure you're
running the UI as the **same user** the engine was installed for — the socket is
credential-gated to the owner UID baked in at install time (and to root). The
app reconnects on its own once the engine is running.

**No tray icon on GNOME.**
GNOME hides AppIndicator tray icons by default. Install and enable the
**"AppIndicator and KStatusNotifierItem Support"** GNOME Shell extension, then
relaunch Sluice (or toggle the extension off and on). The app still runs without
the extension — reopen the window via **Sluice** in the app menu.

**Tray-menu entries show up blank.**
This is an intermittent GNOME AppIndicator quirk (it occasionally drops menu
labels after a Shell reload), not a Sluice bug — the entries are still
clickable. Sluice re-publishes the tray menu shortly after startup and on window
focus to repopulate them. If they're still blank, toggle the AppIndicator
extension off and on, or relaunch the app.

**`protoc` not found / build fails at the protobuf step.**
The protobuf compiler is required to generate the gRPC code. Run
`./install.sh` (without `--skip-deps`) or `just setup` to install it, or install
`protobuf-compiler` from your package manager and re-run.

**eBPF build fails (`bpf-linker` / nightly / `rust-src`).**
The eBPF object needs a nightly toolchain with the `rust-src` component and the
`bpf-linker` tool. A normal `./install.sh` provisions all three; if you used
`--skip-deps`, install them manually:

```bash
rustup toolchain install nightly
rustup component add rust-src --toolchain nightly
cargo install bpf-linker
```

**A block is causing problems — I need the network back now.**
Stop the engine; enforcement detaches immediately and traffic flows:

```bash
sudo systemctl stop sluice-engine
```

Your rules are kept and re-apply when you start it again. Remove or edit the
offending rule from the UI's rules panel before restarting.

**I want to start completely clean.**
Uninstall with data removal, then reinstall:

```bash
./uninstall.sh --purge
./install.sh
```
