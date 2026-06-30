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
  - [Install from a prebuilt package (recommended)](#install-from-a-prebuilt-package-recommended)
  - [Install from source (`./install.sh`)](#install-from-source-installsh)
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

Sluice is **two parts with a clean privilege split**, shipped together in a
single package:

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

Both halves ship in **one combined Debian package named `sluice`** — the desktop
UI plus the **prebuilt** engine (the eBPF object and the loader binary) and the
engine's systemd unit. Installing that single package sets up both. The
recommended way is a prebuilt `.deb` from a release, which needs no build
toolchain at all; the from-source `./install.sh` path builds the very same
package and installs it.

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
- **`sudo` access.** Installing the package needs root (via `sudo apt`), as do
  managing the systemd service and recovery. The from-source `./install.sh` runs
  as your normal user and calls `sudo` only for the steps that genuinely need it
  (installing system packages and the `.deb`). **Do not run `./install.sh` as
  root** — it will refuse, because the UI is per-user and the engine records the
  invoking user's UID as the authorized owner of the control socket (in
  `/etc/sluice/engine.env`).
- **Internet access at install time** — the prebuilt package pulls its runtime
  libraries via `apt`; the from-source path additionally pulls the build
  toolchain. At *runtime* Sluice makes no outbound calls of its own.

> **Build prerequisites apply only to the from-source path.** The recommended
> prebuilt `.deb` needs **none** of them — the engine ships prebuilt inside the
> package, and `apt` pulls the runtime libraries automatically (the package
> `Depends` on `libwebkit2gtk-4.1-0` and `libgtk-3-0`, and `Recommends`
> `nftables`, used for inbound enforcement). Only the from-source `./install.sh`
> needs the build toolchain (the installer provisions it for you on Ubuntu — see
> the step-by-step below): a Rust toolchain (stable, plus a nightly with
> `rust-src` and `bpf-linker` for the eBPF object), `protoc` (the protobuf
> compiler, for the gRPC contract), and the desktop build libraries (WebKitGTK
> and friends).

For the GNOME system tray to show the Sluice tray icon, you'll also want the
**"AppIndicator and KStatusNotifierItem Support"** GNOME Shell extension — see
[The system tray](#the-system-tray-gnome). The app runs fine without it; only
the tray icon is affected.

## Installation

### Install from a prebuilt package (recommended)

For normal use, download the prebuilt Debian package from the latest GitHub
release and install it with `apt` — **no build toolchain required**:

```bash
sudo apt install ./sluice_<version>_amd64.deb
```

This is the easiest path and needs **none** of the build prerequisites (no Rust,
no nightly, no `bpf-linker`, no `protoc`, no WebKitGTK *dev* libraries): the
engine ships prebuilt inside the package, and `apt` pulls the runtime libraries
it depends on automatically (`libwebkit2gtk-4.1-0`, `libgtk-3-0`; it also
recommends `nftables`, used for inbound enforcement).

The package is a single combined bundle named **`sluice`** — it contains both
halves (the desktop UI and the prebuilt engine + its systemd unit). On install,
the package's maintainer scripts do all the setup for you:

- They install, enable, and start the root `sluice-engine` systemd service (the
  unit lands at `/usr/lib/systemd/system/sluice-engine.service`).
- They resolve the **authorized owner UID** — the user allowed to drive the
  engine over its control socket — and record it in **`/etc/sluice/engine.env`**
  as `SLUICE_OWNER_UID=<uid>`. The scripts prefer the user behind `sudo`/`pkexec`,
  falling back to the first regular login user (UID ≥ 1000). To hand the engine
  to a different user later, edit `/etc/sluice/engine.env` and run
  `sudo systemctl restart sluice-engine`.

When it finishes, launch **Sluice** from your app menu (or run `sluice-ui`).

### Install from source (`./install.sh`)

If you're working from a checkout of the repository (developers, or to install a
version you've built yourself), run:

```bash
./install.sh
```

Run it **as your normal user** — it will `sudo` only where it needs to. The
script builds both halves from source, stages the freshly-built engine artifacts
into `crates/sluice-ui/dist-engine/`, builds the **same single combined `.deb`**
that release users get, and installs it. So the from-source path lands in exactly
the same place as the prebuilt path — one `sluice` package containing the engine
+ UI, with the same maintainer scripts setting up the service and the owner UID —
it just compiles everything first. On a fresh machine it also provisions the
build toolchain (a first run that compiles the eBPF object, the engine, and the
Tauri-bundled UI takes a while — subsequent runs are faster).

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
./install.sh              Full from-source install: prerequisites, build + stage the
                          engine, build the combined .deb, install it
./install.sh --skip-deps  Skip the prerequisite/toolchain step (already set up)
./install.sh --engine     Developer fast-path: build + install ONLY the engine
                          (via engine/install.sh; no .deb rebuild)
./install.sh --ui         Rebuild + reinstall the combined .deb (implies --skip-deps)
./install.sh --help       Show usage and exit
```

- `--skip-deps` skips the toolchain provisioning step. Use it when you've
  already run a full install once, or you manage the Rust/`protoc`/WebKitGTK
  prerequisites yourself, and just want to rebuild and reinstall.
- `--engine` is a developer fast-path: it builds the eBPF object and the engine
  loader and (re)installs **only** the engine directly via `engine/install.sh` —
  with **no `.deb` rebuild**. Note this dev path writes the unit to
  `/etc/systemd/system/sluice-engine.service` (the source/dev location, not the
  packaged `/usr/lib/systemd/system/` one) and the owner UID to
  `/etc/sluice/engine.env`. Handy for quick engine iteration.
- `--ui` rebuilds and reinstalls the combined `.deb`. It implies `--skip-deps`,
  and reuses already-staged engine artifacts in `crates/sluice-ui/dist-engine/`
  if they're present (otherwise it builds them first).

### What the installer does, step by step

A full `./install.sh` (from source) runs these phases:

1. **Prerequisites — the build toolchain.** Runs the repo's `scripts/setup.sh`,
   which (on Ubuntu, via `apt`) installs the protobuf compiler, `pkg-config`,
   `build-essential`, and the desktop build libraries (WebKitGTK, libsoup,
   librsvg, the AppIndicator dev library, and OpenSSL dev). It also ensures Rust
   is present via `rustup`. The installer then provisions the **eBPF toolchain**:
   a **nightly** Rust toolchain with the **`rust-src`** component, plus
   **`bpf-linker`** (installed with `cargo install bpf-linker` if it's missing —
   this compile takes a few minutes the first time).

2. **Build and stage the engine.** Compiles the eBPF object (`engine/ebpf`,
   pinned to nightly by its own toolchain file) and the root loader
   (`engine/loader`) in release mode, then stages both artifacts (the
   `sluice-ebpf` object and the `sluice-engine` loader binary) into
   `crates/sluice-ui/dist-engine/` so they can be bundled into the package.

3. **Build the combined `.deb`.** Ensures the Tauri CLI is present
   (`cargo install tauri-cli` if needed) and builds a single Debian package named
   **`sluice`** with `cargo tauri build --bundles deb`. The package contains the
   desktop UI (the `sluice-ui` binary, a **"Sluice"** application-menu entry, and
   the icons), the prebuilt engine artifacts staged in step 2, and the engine's
   systemd unit. The `.deb` is left in the build tree at
   **`target/release/bundle/deb/`** if you want to copy it to another machine.

4. **Install the `.deb`.** Installs the package with `apt`/`dpkg`. The package's
   maintainer scripts then do the engine setup automatically — exactly as in the
   prebuilt path: they install, enable, and start the root `sluice-engine`
   service (unit at `/usr/lib/systemd/system/sluice-engine.service`) and record
   your UID as the authorized owner of the control socket in
   `/etc/sluice/engine.env`. The unit sets the runtime directory (`/run/sluice`),
   the state directory (`/var/lib/sluice`), the socket path, the rules-file path,
   and `Restart=always` so the engine comes back after a crash or reboot.

`--ui` runs phases 2–4 only (skipping prerequisites), and reuses already-staged
engine artifacts from `crates/sluice-ui/dist-engine/` if they're present. With
`--skip-deps`, phase 1 is skipped. `--engine` takes a different route entirely:
instead of building the `.deb`, it builds and installs **only** the engine
directly via `engine/install.sh`, which writes the unit to
`/etc/systemd/system/sluice-engine.service` and the owner UID to
`/etc/sluice/engine.env` — a quick path for engine-only iteration that skips the
package rebuild.

> **One firewall at a time.** The engine hooks the kernel's `cgroup/connect`
> path. If you have another always-on connection firewall enabled at boot,
> running two will fight over the same hook — the engine installer warns you and
> prints the command to disable the conflicting service so Sluice is the only one
> managing connections.

### Manual / split install

You don't have to use the top-level `install.sh`. The pieces can be built and
installed independently, which is the typical developer workflow.

**Engine only (dev):**

```bash
# Build the eBPF object (nightly) and the loader (stable):
( cd engine/ebpf && cargo build --release )
( cd engine/loader && cargo build --release )

# Install + start the root service. This dev path writes the unit to
# /etc/systemd/system/sluice-engine.service (the source location) and records
# your UID as the socket owner in /etc/sluice/engine.env:
sudo engine/install.sh
sudo systemctl enable --now sluice-engine
```

(`just engine-build` and `just engine-install` wrap exactly these commands, and
`./install.sh --engine` runs this same engine-only path.)

**The combined package:**

The installable `.deb` bundles the prebuilt engine artifacts staged under
`crates/sluice-ui/dist-engine/`; `./install.sh` (or `./install.sh --ui`) stages
them and builds the package for you. The underlying build step is:

```bash
# Build the combined .deb (output under target/release/bundle/deb/):
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
| `/usr/lib/systemd/system/sluice-engine.service` | root | The engine's systemd unit (packaged install). The dev/source `engine/install.sh` path writes it to `/etc/systemd/system/sluice-engine.service` instead. |
| `/etc/sluice/engine.env` | root | Records the engine's authorized owner UID (`SLUICE_OWNER_UID=`); read by the unit via `EnvironmentFile`. |
| `/run/sluice/engine.sock` | root | The hardened control socket (dir `0700`, socket `0600`, peer-credential gated to the owner UID or root). |
| `/var/lib/sluice/rules.json` | root | The persisted rule store (your blocks/allows). |
| `/usr/bin/sluice-ui` | system | The desktop app binary (installed by the `sluice` package). |
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

**In-app (easiest).** In **Settings → Updates**, click **Check for updates**; if a
newer release exists, an **Update now** button downloads that release's `.deb`,
verifies its SHA-256, and installs it via a polkit password prompt (the app never
runs as root). When it finishes, click **Restart now** to launch the new version.
This is opt-in and is the only feature that reaches the network. If you'd rather do
it by hand, the two manual paths below do the same thing.

Any update (in-app or manual) is **rollback-protected**: the package snapshots the
current engine first, and if the new engine fails to attach within ~15s it restores
the previous, known-good engine rather than leaving the firewall down (only the
engine rolls back; the app stays updated). Recovery is always
`sudo systemctl stop sluice-engine`.

**Prebuilt package (recommended manual path).** Download the newer release `.deb` and
install it over the top:

```bash
sudo apt install ./sluice_<version>_amd64.deb
```

`apt` performs a clean package upgrade. Your data is preserved
(`/var/lib/sluice/rules.json` and `~/.local/share/sluice`), and the package
restarts the engine service for you. Restart the desktop app to pick up the new
UI build.

**From source.** Rebuild and reinstall from a newer checkout:

```bash
git pull
./install.sh
```

Re-running `./install.sh` is safe and idempotent. To save time during engine
iteration you can target just the engine with `./install.sh --engine`, or rebuild
just the package with `./install.sh --ui`.

Your data is preserved across updates either way: the engine's `rules.json` and
the UI's history database are not touched by an upgrade. After an update the
engine service is restarted automatically; restart the desktop app yourself to
pick up a new UI build.

## Uninstalling

To remove Sluice, run the uninstaller as your normal user:

```bash
./uninstall.sh
```

It removes the `sluice` package (engine + UI) and also cleans up a stale
source-installed engine unit (`/etc/systemd/system/sluice-engine.service`) if one
is present. Stopping the engine reopens inbound traffic, so by the end the network
is back to normal. By default the uninstaller is interactive and **prompts before
deleting your data** (the engine rule store and the UI history).

Flags control the data prompt:

```bash
./uninstall.sh           # Remove Sluice; prompt about your data
./uninstall.sh --purge   # Remove everything including data, no prompts
./uninstall.sh --keep    # Remove Sluice, keep all data, no prompts
./uninstall.sh --help    # Show usage and exit
```

You can also remove the package directly with `apt`:

```bash
sudo apt remove sluice    # Remove the package; keeps /etc/sluice and /var/lib/sluice
sudo apt purge sluice     # Also remove /etc/sluice — but deliberately keeps
                          # /var/lib/sluice (your rule store), so an accidental
                          # purge can't wipe your firewall rules
```

- **Always removed:** the `sluice` package (engine + UI) and its systemd unit.
  Stopping the engine on removal reopens inbound traffic.
- **Your data** (kept unless you choose otherwise): `/var/lib/sluice` (the rule
  store) and `~/.local/share/sluice` (the UI history). Even `apt purge` keeps
  `/var/lib/sluice`.

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
(`0.1.8`).

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
credential-gated to the owner UID recorded in `/etc/sluice/engine.env` (and to
root). To hand the engine to a different user, edit that file's
`SLUICE_OWNER_UID` and run `sudo systemctl restart sluice-engine`. The app
reconnects on its own once the engine is running.

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
This only applies when building from source — the prebuilt `.deb` compiles
nothing. The protobuf compiler is required to generate the gRPC code. Run
`./install.sh` (without `--skip-deps`) or `just setup` to install it, or install
`protobuf-compiler` from your package manager and re-run.

**eBPF build fails (`bpf-linker` / nightly / `rust-src`).**
Again, only relevant to a from-source build. The eBPF object needs a nightly
toolchain with the `rust-src` component and the `bpf-linker` tool. A normal
`./install.sh` provisions all three; if you used `--skip-deps`, install them
manually:

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
