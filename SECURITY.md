# Security Policy

Sluice is an application firewall and network monitor for Ubuntu (GNOME). It enforces firewall rules in the kernel from a small root daemon and presents everything through an unprivileged desktop app. Because Sluice runs with elevated privilege on the host and sits on the network path, its security posture is a first-class design concern, not an afterthought.

This document describes Sluice's threat model, its trust boundary, the fail-safe principles it is built around, how data is stored at rest, and how to report a vulnerability.

> Sluice is maintained as a focused, small-team project. Security reports are taken seriously and acted on, but please calibrate expectations to a best-effort, no-SLA project.

## Supported Versions

Only the **latest released version** receives security patches. Before reporting, please confirm the issue reproduces on the latest release. The installed version is recorded in the [`VERSION`](VERSION) file at the root of the install tree.

## Architecture and Trust Boundary

Sluice is split into two components with deliberately asymmetric privilege:

### 1. The engine — `sluice-engine` (root)

A small systemd-managed daemon that runs as **root** because loading eBPF programs and writing the kernel firewall require it. The engine:

- Attaches an eBPF `cgroup/connect4` and `cgroup/connect6` program to the unified cgroup-v2 root, so **every** task on the host is covered. Outbound connections are matched against in-kernel denylist maps (IPv4 and IPv6) and blocked **in the kernel** before the connection leaves the machine. The same program emits a record for every connection over a ring buffer, adding roughly a microsecond of latency per connection.
- Enforces inbound policy through an nftables table (`inet sluice`): an opt-in default-deny input chain with an explicit allow-list, plus a passive conntrack observer that surfaces new incoming connections in the feed.
- Runs a passive packet-capture DNS observer that labels destination IPs with the hostnames the host resolved, and enriches connection records with process and container attribution read from `/proc` and the container runtime.
- Persists rules to `/var/lib/sluice/rules.json` and reconciles them on `SIGHUP`.
- Exposes a gRPC service **only** over a local Unix-domain socket — there is no TCP listener and no network-facing port.

### 2. The UI — the desktop app (unprivileged)

A desktop application that runs as the **logged-in user, never as root**. It is a gRPC *client* of the engine. The UI:

- Renders the live feed and the analysis views (Usage, Apps, Destinations, Security, Inbound), with filtering and a bandwidth graph.
- Requests rule changes — the **engine** performs every privileged action; the UI itself touches nothing privileged.
- Keeps a local SQLite history database of connection decisions under the user's data directory.
- Ships a static webview frontend with no JavaScript build step and a strict Content-Security-Policy.

### The trust boundary

The single trust boundary is the **Unix-domain socket** between the root engine and the unprivileged UI:

```
root engine  ──  /run/sluice/engine.sock  ──  owner-uid UI
(privileged)        UDS, no network            (unprivileged)
```

That socket is hardened on multiple, independent layers:

- **No network exposure.** The engine binds a Unix-domain socket only. There is no TCP socket, no loopback listener, and nothing that can be reached from another host on the network.
- **Restrictive filesystem permissions.** The socket's parent directory (`/run/sluice`) is created and traversable only as intended; the socket file itself is set to mode `0600` and `chown`ed to the owner's uid, so only that user (and root) can even open it. Sluice never relaxes permissions on a pre-existing shared directory it did not create.
- **Peer-credential enforcement.** As defence in depth beyond the filesystem permissions, every accepted connection is checked with `SO_PEERCRED`. The engine admits only connections whose peer uid is the owner's uid or root; any other uid, or any failure to read the peer credentials, is logged and the connection is dropped.
- **Disabled when ambiguous.** If the engine cannot determine which uid owns the session, the UI link is disabled entirely rather than served to an unknown principal.

The result is that an unprivileged process belonging to a *different* user cannot read the feed, see another user's traffic, or write firewall rules, and nothing off-box can reach the control plane at all.

## Threat Model

Sluice is a **single-user host firewall**. The assumed deployment is one trusted owner on a personal or workstation machine. Within that model:

**What Sluice defends against:**

- An ordinary process on the host opening connections you did not intend — Sluice surfaces them and lets you block them, enforced in the kernel.
- Another local user (or any process not running as the owner or root) attempting to read your connection history or quietly change your firewall rules through the control plane.
- Anything off-box: there is no network listener, so there is no remote attack surface on the control plane.
- Malformed or hostile network and kernel input reaching the engine's parsers (DNS responses, conntrack events, process and container metadata). These are parsed in safe Rust with consistent bounds checking, message-length and compression-pointer guards, and no panic-on-malformed-input path.
- Hostile strings in connection data reaching the webview. Every connection-derived field (hostnames, process names, paths, arguments, container labels, peer addresses, protocol, and the "why") is HTML-escaped at every render site, behind a strict CSP with no remote origins and no dynamic script execution.

**What is explicitly out of scope:**

- **The owner acting against their own machine.** The owner has physical and administrative control: they can stop the service, edit the root-owned rule file directly, or change their own configuration. Sluice does not attempt to be tamper-proof against its own administrator — by design, the owner must always be able to take back control of their network (see *Fail-Safe Principles*).
- **Code already running as the owner defeating a default-allow host firewall.** Sluice's default posture is allow-with-a-denylist (see below). Software running as you can take many actions; Sluice's job is to make outbound activity visible and to enforce the specific blocks you choose, not to sandbox arbitrary owner-privileged code.
- **Issues in third-party dependencies or the host OS.** Please report those to the relevant upstream project.
- **Best-practice deviations with no demonstrable exploitation path.**

## Fail-Safe Principles

Sluice is a firewall: a bug that *over-blocks* can take a machine offline. The design therefore biases toward never stranding the host.

- **Default-allow with a denylist — no lockout.** The engine enforces only the rules you have explicitly added; everything not on the denylist is allowed. There is no default-deny outbound mode that could lock you out of your own network if the engine, the UI, or a rule is wrong.
- **Monitor by default — traffic is never held.** Out of the box, Sluice observes and reports; it does not pause or queue connections waiting for a decision. Connections are never held hostage to the UI being responsive.
- **Critical destinations cannot be blocked.** Both the UI and the engine refuse to write a block rule that would strand the machine. The UI rejects attempts to block `localhost`, the `.localhost` suffix, loopback addresses, or an empty (too-broad) host. The engine independently re-checks every rule it receives over the control plane and refuses loopback and the unspecified address (`0.0.0.0` / `::`), so even a bypassed UI guard cannot cut off local services or the DNS stub. (Rules written directly to the root-owned rule file by the administrator are trusted as-is.)
- **Inbound enforcement is structurally safe.** When the inbound default-deny chain is enabled, `established`/`related` traffic and loopback are always accepted, so enabling inbound enforcement never breaks return traffic for connections you initiated. The nftables ruleset is built from a fixed template into which only validated `u16` port numbers are interpolated — never an untrusted string — and is applied atomically with no shell involved. The table is torn down on stop, on disabling enforcement, and on exit, reopening inbound traffic.
- **There is always a recovery path.** If anything goes wrong, stopping the engine immediately removes all enforcement and reopens the network:

  ```bash
  sudo systemctl stop sluice-engine
  ```

  Stopping the service detaches the eBPF programs and removes the inbound nftables table, returning the host to its normal, unfiltered state.

## Data at Rest

Sluice stores two kinds of data locally, and both are owner-readable only:

- **Connection history (UI).** A SQLite database under the user's data directory (typically `~/.local/share/sluice`) holds resolved connection decisions so the feed and analysis views survive restarts. Because this is personal data (hostnames, process paths, IP addresses), the database — and its journal/WAL sidecars — are set to mode `0600` inside a `0700` data directory, so only the owner can read it. If the data directory cannot be opened, the UI falls back to an in-memory store rather than writing to an unsuitable location. History is bounded by a rolling row cap and an optional time-based retention window, and can be cleared from the app at any time.
- **Firewall rules (engine).** The rule store at `/var/lib/sluice/rules.json` is written `0600` and is root-owned. It contains only firewall rules — destination addresses and ports — and nothing privileged beyond that.

The two stores are deliberately separate: clearing your history never affects your firewall rules, and vice versa.

## Quiet by Default

Sluice makes **no network calls of its own**. It does not phone home, ship telemetry, check for updates automatically, or contact any remote service in the background. A correctly running Sluice should be effectively invisible in its own feed.

- The product contains no live HTTP client in any active code path. The engine's DNS and conntrack observers are strictly read-only and never transmit.
- Local helpers used to describe processes shell out only to local system tools, with explicit argument terminators and no shell interpretation — there is no command injection and no network access.
- The only outbound action Sluice ever takes is fully user-initiated and clearly labelled in the UI: an explicit "look up host" button that opens an external reputation lookup in your browser. Nothing happens until you click it.

Any future feature that would make network calls will be opt-in and disclosed in the UI.

## Updates and Release Signing

Updates are opt-in and user-initiated — the update check is off by default, and nothing is downloaded or installed without an explicit click.

Release artifacts are **cryptographically signed**, so the in-app updater verifies a download's **authenticity**, not just its integrity:

- Each release `.deb` is signed with an **Ed25519** key (via [minisign](https://jedisct1.github.io/minisign/)); the detached signature ships as the `.minisig` asset alongside the `.deb` and its `.sha256`.
- The **secret key is held offline** and never appears in the repository or CI. The matching **public key is embedded in the app** (`crates/sluice-ui/sluice-release.pub`) and published here:

  ```
  RWSn2Bxeyd35sx6sBsoLIO4TsQYVqyMxtCo/WyGddd20bSCp6gmi3P4Q
  ```

  (minisign key ID `B3F9DDC95E1CD8A7`)

- When you use **Update now**, the updater downloads the `.deb` + `.minisig`, **verifies the signature against the embedded public key first** (in-process, pure Rust — no external tools required), and **refuses to install** on a missing or invalid signature. It then checks the SHA-256 and installs via a polkit prompt — the app never runs as root. A tampered package is rejected even if its checksum was swapped to match.

You can verify a download yourself:

```bash
minisign -Vm Sluice_<version>_amd64.deb -P RWSn2Bxeyd35sx6sBsoLIO4TsQYVqyMxtCo/WyGddd20bSCp6gmi3P4Q
```

**Key rotation.** If the signing key is ever lost or compromised, a new public key will be published here and embedded in a new release. Because an older app only trusts the older key, that transition release must be installed manually (downloaded and verified against the new key) once; auto-update resumes afterward.

## Security Review

The Sluice codebase — the root engine, the unprivileged UI, and the gRPC contract between them — has undergone an adversarial source-level security review against the threat model above. The review examined the privilege boundary and peer-credential gate, every parser that handles untrusted network or kernel input, the nftables rule generation, the webview escaping and CSP, and the quiet-by-default egress posture.

The review found **no critical or high-severity issues**. The privilege boundary, input parsers, ruleset generation, and webview escaping were assessed as sound. Findings of lower severity have been addressed; notably, the review surfaced an IPv6 enforcement gap that has since been closed — outbound block rules are now enforced in-kernel for both IPv4 and IPv6, verified on a live kernel.

This is not a guarantee that no issues remain. If you find one, please report it.

## Reporting a Vulnerability

**Please do not file a public issue for security vulnerabilities.** Use GitHub's private vulnerability reporting:

1. Go to the **Security** tab of the [Sluice repository](https://github.com/preston-peterson/sluice).
2. Click **Report a vulnerability**.
3. Submit the form.

A useful report includes:

- The Sluice version (the [`VERSION`](VERSION) file, or `systemctl status sluice-engine`).
- A clear description of the issue and which component it affects (engine, UI, or the control-plane boundary).
- Steps to reproduce, if possible.
- The potential impact — privilege escalation, cross-user data exposure, firewall bypass or unauthorized rule write, denial of service of the root engine via crafted input, silent network egress, or webview script injection.
- A proof-of-concept, if you are comfortable sharing one.

### What to expect

- **Acknowledgment:** best effort, typically within a week. There is no SLA.
- **Triage:** the report is assessed for severity, reproducibility, and scope against the threat model.
- **Fix:** confirmed, in-scope issues are patched in the next release; serious issues may get an out-of-band patch.
- **Disclosure:** coordinated disclosure is preferred. The fix lands in the changelog without exploit details until users have had a reasonable window to update.

## What Counts as a Vulnerability

**In scope** (please report):

- Privilege escalation, or any path by which a non-owner local process or an off-box party can reach the engine control plane.
- A firewall bypass: outbound traffic that should be blocked by a rule but is not enforced, or an unauthorized write/removal of a firewall rule.
- Cross-user data exposure — reading another user's connection history or feed.
- Memory unsafety, a panic, or a denial of service of the root engine reachable through crafted network or kernel input (DNS, conntrack, process/container metadata, the ring buffer).
- Silent or undisclosed network egress by Sluice itself.
- Script injection (XSS) in the webview driven by attacker-controlled connection data.
- Command injection, path traversal, or arbitrary file write in the Sluice codebase.

**Out of scope** (report upstream or to the appropriate party):

- The owner being able to stop the service, edit their own root-owned rule file, or otherwise administer their own machine — this is by design and is the deliberate recovery path.
- Default-allow / monitor being the default posture — a deliberate fail-safe choice, see *Fail-Safe Principles*.
- `established`/`related` and loopback always being accepted on the inbound chain — structural and required for working return traffic.
- Issues in third-party dependencies or in the host OS — please report to the relevant upstream project.
- Theoretical issues with no clear exploitation path, or best-practice deviations that are not exploitable.

## Hardening Suggestions

A few practices worth following on any Sluice deployment:

- Keep the host system patched — Ubuntu receives regular security updates.
- Keep Sluice itself on the latest release, where security fixes land.
- Remember the recovery path: `sudo systemctl stop sluice-engine` immediately removes all enforcement if a rule or update ever causes trouble.

Thanks for helping keep Sluice secure.
