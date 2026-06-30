# Changelog

All notable changes to **Sluice** are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
follows [Semantic Versioning](https://semver.org/).

## [Unreleased]

Nothing yet.

## [0.1.11] — 2026-06-30

### Changed

- The in-app updater's success state now **replaces** the Check/Update buttons with a highlighted
  "ready — restart to apply" line and a pulsing **Restart now** button, so it's clear the update
  applied and what to do next; after the restart the row returns to normal.

## [0.1.10] — 2026-06-30

### Changed

- Validation release exercising the 0.1.9 in-app updater end-to-end (download → verify → install →
  restart). No functional changes since 0.1.9.

## [0.1.9] — 2026-06-30

### Added

- **In-app update (opt-in).** When Settings → Updates finds a newer release, an **Update now**
  button downloads that release's `.deb`, **verifies its SHA-256**, and installs it via polkit
  (`pkexec`) — user-confirmed — then offers to restart. It's off by default and remains the only
  feature that reaches the network (SEC-007); the privileged install goes through polkit, never as
  root (SEC-001). Integrity is checksum-verified against the GitHub release; signed artifacts are a
  planned follow-up.

## [0.1.8] — 2026-06-29

### Changed

- **One combined package — install with no build toolchain.** Sluice now ships as a single
  Debian package (`sluice`) containing *both* halves: the desktop UI and the **prebuilt** engine
  (eBPF object + loader) plus its systemd unit. `sudo apt install ./sluice_<version>_amd64.deb`
  installs the whole product and needs **no** Rust/nightly/`bpf-linker`/`protoc` — the engine
  ships prebuilt inside the package. Installing enables the root engine service; removing it stops
  the engine (reopening inbound) and cleans up. `apt purge` keeps your rule store
  (`/var/lib/sluice`) so it can't be wiped by accident.
- **Owner UID moved out of the systemd unit.** The user authorized to drive the engine over its
  control socket is now recorded in `/etc/sluice/engine.env` (resolved at install time) instead of
  being baked into the unit, so the unit is identical for packaged and from-source installs. Edit
  that file and `systemctl restart sluice-engine` to change it.
- **`./install.sh` is now the from-source path** and installs via the same combined `.deb`;
  `scripts/package-release.sh` builds the engine and bundles the prebuilt artifacts into the
  release `.deb`.

### Added

- `Recommends: nftables` on the package (used for inbound enforcement).

## [0.1.7] — 2026-06-28

### Added

- The installed version is now shown in the app header (top-right).

## [0.1.6] — 2026-06-28

### Added

- **Opt-in update check** (Settings → Updates): checks GitHub Releases for a newer version and
  shows it inline with a link to the notes. Off by default; when enabled it checks once at launch.
  This is the only network request Sluice makes — it's explicit and shows up in your own feed.
- **Release packaging** (`scripts/package-release.sh` / `just release`): builds the `.deb` + a
  SHA-256 checksum and can publish a GitHub release.

### Fixed

- The window could come up unresponsive (dead minimize/maximize/close and no input) on some
  Wayland systems. Sluice now disables WebKitGTK's DMABUF renderer at startup — the standard fix —
  so it launches normally without any environment tweaks.

## [0.1.5] — 2026-06-26

The first cut of Sluice: a desktop application firewall and network monitor for
Ubuntu (GNOME) that watches and controls **both outbound and inbound** traffic,
with a live decision feed and a two-click allow/block workflow. Enforcement runs
in a small root engine; the desktop app is unprivileged and never holds your
traffic by default.

### Added

#### Outbound firewall (in-kernel)

- **eBPF outbound enforcement for IPv4 and IPv6.** A `cgroup/connect` program
  denies outbound connections directly in the kernel from in-kernel rule maps,
  adding roughly a microsecond per connection. Every connection — allowed or
  blocked — is streamed up to the desktop app over a ring buffer.
- **Persistent, hot-reloaded rules.** Block rules (by destination IP, optionally
  scoped to a port) are saved to disk by the root engine and reconciled live, so
  your firewall posture survives restarts and reloads without dropping traffic.

#### Inbound control (nftables)

- **Inbound firewall with two postures.** Start in **observe** mode — see incoming
  connection attempts without changing anything — then switch to **enforce**, which
  installs a default-deny input policy plus an allow-list. Established/related and
  loopback traffic are always accepted, so enabling enforcement never breaks return
  traffic or local services.
- **Inbound view.** A dedicated view for incoming activity with an editable
  allow-list and per-port drill-down, so you can see what's knocking and decide
  what to let in.

#### Live feed

- **Coalesced live activity feed.** Connections are grouped and counted as they
  happen, so a busy machine stays readable.
- **Filters.** Free-text search plus verdict, protocol/category, and local-traffic
  filters narrow the feed to exactly what you're looking at.
- **Time-window views.** Switch between live, last hour, today, and 7-day windows.
- **Expandable row detail.** Each entry opens to show the process (pid/uid, path,
  name, command line), source and destination, protocol, the verdict and why,
  destination country (offline geo lookup), and on-demand reverse DNS.
- **Process explainer.** A local-only "what is this?" lookup helps you recognize an
  unfamiliar process without leaving the app.
- **Project grouping.** Activity is grouped by the launching application, with
  collapsible sections and aggregate summaries.
- **Hostnames and container labels.** A passive DNS observer labels destinations
  with their hostnames, and containerized processes are tagged with their container
  name.

#### Decide in place

- **Two-click allow/block.** Act on any connection with a scope × duration picker,
  a confirmation step, and a critical-host safelist that refuses to block loopback
  or other essential destinations.
- **Rules panel.** Review and remove your active firewall rules in one place.

#### Views and insights

- **Apps and Destinations.** Permission-style panels that show each app's and each
  destination's real rule posture, with allow/block right there.
- **Usage.** A top-talkers view ranking the most-active hosts or apps for a chosen
  window, with an allowed/blocked split, sortable columns, search, and per-row
  drill-down.
- **Security.** An event log of security-relevant events with severity, search,
  per-event detail, and a clear action.

#### Desktop integration

- **System tray and notifications.** A tray icon (show/quit; closing hides to tray)
  and desktop notifications for new apps and engine status changes.
- **Engine-status indicator.** See at a glance whether the engine is connected.
- **Live bandwidth graph.** A real-time total throughput graph (in/out) with a
  selectable window and a bits/bytes unit toggle, fed by an unprivileged local
  sampler.
- **Persisted history.** Resolved feed activity is stored locally in SQLite so the
  feed survives restarts; it's user-clearable.

#### Install

- **One-command installer.** `./install.sh` builds and installs both halves — the
  root engine as a systemd service and the per-user desktop app — pulling in the
  build toolchain as needed. `./uninstall.sh` removes everything.
- **Debian package.** A `.deb` bundle installs the desktop app, its launcher, and
  its icons.

### Security

- **Privilege split.** Only the engine runs as root and does the privileged work;
  the desktop app is fully unprivileged, runs as your user, and makes no network
  calls of its own.
- **Hardened engine socket.** The engine serves the UI over a Unix socket with a
  `0700` directory and a `0600` socket, gated by peer-credential checks to the
  owner's user (or root).
- **Quiet by default.** Sluice doesn't phone home; it stays quiet in its own feed.
- **Safe by default, no lockout.** The default posture is monitor — connections are
  never held — and the engine is default-allow with a denylist, so there's no way
  to lock yourself out. Recovery is always `sudo systemctl stop sluice-engine`.
- **Local data at rest.** History and configuration are stored with `0600`
  permissions in a `0700` directory.

[Unreleased]: https://github.com/preston-peterson/sluice/compare/v0.1.11...HEAD
[0.1.11]: https://github.com/preston-peterson/sluice/compare/v0.1.10...v0.1.11
[0.1.10]: https://github.com/preston-peterson/sluice/compare/v0.1.9...v0.1.10
[0.1.9]: https://github.com/preston-peterson/sluice/compare/v0.1.8...v0.1.9
[0.1.8]: https://github.com/preston-peterson/sluice/compare/v0.1.7...v0.1.8
[0.1.7]: https://github.com/preston-peterson/sluice/compare/v0.1.6...v0.1.7
[0.1.6]: https://github.com/preston-peterson/sluice/compare/v0.1.5...v0.1.6
[0.1.5]: https://github.com/preston-peterson/sluice/releases/tag/v0.1.5
