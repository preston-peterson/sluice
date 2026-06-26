# Contributing to Sluice

Thanks for taking an interest in Sluice. This guide covers how to get a
working build, the change workflow, the gates your work has to pass before
it's ready to submit, and the conventions the codebase already follows.

Sluice is firewall-adjacent software with a deliberate privilege split — a
small **root** engine that enforces in the kernel, and an **unprivileged**
desktop UI that drives it. That boundary is the most important thing about
the project, so before you change anything that touches enforcement,
transport, or the privilege line, please read
[Security & the privilege boundary](#security--the-privilege-boundary)
below and skim the [SECURITY.md](SECURITY.md) threat model.

---

## Table of contents

- [Ground rules](#ground-rules)
- [Getting set up](#getting-set-up)
- [The two-workspace build](#the-two-workspace-build)
- [The change workflow](#the-change-workflow)
- [Gates: what has to pass before you submit](#gates-what-has-to-pass-before-you-submit)
- [Commit message style](#commit-message-style)
- [Code style](#code-style)
- [Security & the privilege boundary](#security--the-privilege-boundary)
- [Reporting bugs](#reporting-bugs)
- [Suggesting features](#suggesting-features)

---

## Ground rules

A few principles guide everything here. They're not bureaucracy — they're
the reasons the project is structured the way it is.

- **Never relax safety to make something pass.** The default posture is
  *monitor* (the engine never holds traffic), and enforcement is
  default-allow plus a denylist, so there's no way to lock yourself out of
  the network. Don't introduce a code path, test fixture, or "temporary"
  global allow/silent mode that breaks those invariants. Recovery is always
  `sudo systemctl stop sluice-engine`, and that must stay true.
- **The UI never runs as root and never enforces.** All privileged work —
  loading the eBPF program, editing kernel rule maps, writing nftables,
  persisting the rule store — happens inside the engine. The UI is a gRPC
  *client*; it asks, the engine acts.
- **Quiet by default.** Sluice makes no network calls of its own. If you add
  a feature that reaches the network (an optional lookup, an update check),
  it must be opt-in and visible to the user — never a silent egress.
- **Small, reviewable changes.** One logical change per branch, with a clear
  commit history. This makes review tractable and keeps the codebase
  internally consistent.

---

## Getting set up

The fastest path to a running build is the one-command installer, which
pulls in the toolchain and system libraries as it goes:

```bash
./install.sh
```

For day-to-day development you'll usually want the dev bootstrap instead,
which installs the build dependencies (Rust toolchain, `protoc`, the
WebKitGTK/Tauri system libraries) and reports on prerequisites without
touching your firewall:

```bash
just setup        # idempotent; installs deps, verifies the toolchain
```

Then the common loop:

```bash
just check          # fmt + clippy + tests for the root workspace (the CI gate)
just ui             # run the desktop app against a running engine
just engine-build   # build the eBPF object + the root loader (nightly toolchain)
```

A full walkthrough of the toolchain, the system libraries, host
prerequisites (cgroup-v2, kernel BTF), and how to run the engine locally
lives in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md). Read that first if `just
setup` reports anything missing. Installation and uninstallation are covered
in [docs/INSTALL.md](docs/INSTALL.md); `./uninstall.sh` removes everything.

### What you need

- **Rust** — the root workspace is pinned via `rust-toolchain.toml` (stable
  1.89). `rustup` installs the pinned channel automatically the first time
  you build inside the repo.
- **`protoc`** — required for the gRPC code generation in `crates/sluice-proto`.
- **Tauri / WebKitGTK system libraries** — required to build and run the
  desktop UI (`crates/sluice-ui`). `just setup` installs these on Ubuntu.
- **For the engine only:** a **nightly** toolchain with the `rust-src`
  component and `bpf-linker` on your `PATH` — see the next section.

---

## The two-workspace build

This is the one non-obvious thing about building Sluice, so it's worth
calling out up front: **the repository contains two separate Cargo
workspaces**, and they build on different toolchains.

| Workspace | Path | Toolchain | What's in it |
|---|---|---|---|
| **Root** | `crates/*` | stable, pinned 1.89 (`rust-toolchain.toml`) | the desktop UI (`sluice-ui`), the shared value types (`sluice-types`), and the gRPC contract + generated stubs (`sluice-proto`) |
| **Engine** | `engine/*` | **nightly** + `rust-src` | the eBPF program (`ebpf`), the root loader daemon (`loader`), and the shared kernel↔userspace ABI (`common`) |

The engine is a detached workspace on purpose: its eBPF crate compiles for a
bare-metal BPF target (`bpfel-unknown-none`) using `-Z build-std=core`, which
needs nightly and `rust-src` — a different world from the host-target crates,
which stay on the stable pin. Each carries its own `rust-toolchain.toml`, so
`cargo` picks the right compiler based on which directory you're in.

To build the engine you'll also need `bpf-linker` on your `PATH`:

```bash
cargo +stable install bpf-linker --locked
```

Install it with `+stable` so it doesn't try to use the engine's nightly pin.
Then:

```bash
just engine-build   # builds the eBPF object, then the host loader
```

`just check` only covers the **root** workspace — that's deliberate (it's the
CI gate and runs on stable everywhere). If your change touches anything under
`engine/`, build the engine separately and run its tests there as well. See
[`engine/README.md`](engine/README.md) for the engine's own build, run, and
test recipes.

---

## The change workflow

1. **Branch off `main`.** Don't commit directly to `main`. Use a short,
   descriptive branch name (e.g. `feed-coalesce-fix`, `inbound-port-drilldown`).
2. **Keep the change focused.** One logical change per branch. If you find
   yourself fixing an unrelated thing along the way, split it into its own
   commit (or its own branch).
3. **Make small, reviewable commits** with clear messages — see
   [Commit message style](#commit-message-style). A reviewer should be able
   to follow the history commit by commit.
4. **Run the gates** (next section) and make sure they're green *before* you
   open a pull request.
5. **Open a pull request against `main`.** Describe what changed and why, and
   call out anything that touches the privilege boundary, the engine, or the
   default safety posture so it gets the attention it deserves. If your change
   is user-visible, a screenshot or short clip helps a lot.

---

## Gates: what has to pass before you submit

The single command that covers the root workspace is:

```bash
just check
```

which runs, and which a reviewer will expect to be green:

```bash
cargo fmt --all -- --check                          # formatting is enforced, not suggested
cargo clippy --workspace --all-targets -- -D warnings   # clippy warnings are errors
cargo test --workspace                              # the test suite must pass
```

A few details:

- **Formatting is a hard gate.** Run `just fmt` (or `cargo fmt --all`) before
  committing; the check fails on any diff.
- **Clippy warnings are treated as errors** (`-D warnings`). Fix them rather
  than suppressing them; reach for `#[allow(...)]` only with a comment
  explaining why.
- **The frontend is plain static JavaScript** — there is no JS build step and
  no bundler. If you edit the webview frontend
  (`crates/sluice-ui/frontend/main.js`), syntax-check it before committing:

  ```bash
  node --check crates/sluice-ui/frontend/main.js
  ```

- **If you touched the engine** (`engine/*`), build it (`just engine-build`)
  and run its tests as well — `just check` does not cover the detached engine
  workspace.

There's also an optional supply-chain audit that's intentionally kept
separate from the main gate (it needs `cargo install cargo-audit` and a
current advisory database):

```bash
just audit   # cargo audit — scans dependencies against the RUSTSEC advisory DB
```

Running it before a dependency bump is good practice but isn't part of the
per-change gate.

---

## Commit message style

Match the existing history. Subjects are **concise, lowercase, and
type-prefixed**, optionally with a scope in parentheses:

```
type(scope): short imperative summary
```

Looking at the log, the common types are `feat`, `fix`, `refactor`, `docs`,
`build`, `chore`, and `ui`, and the scope names the area touched (`feed`,
`engine`, `ui`, `inbound`, `install`, `dests`, `readme`, …). Real examples
from the project:

```
feat(inbound): per-allowed-port drill-down — show the traffic a port carries
fix(engine): enforce IPv6 outbound rules
refactor(ui): remove the dormant UI-service path — sluice-ui is engine-only
docs(readme): tighten the architecture diagram
feat(install): one-command install.sh + uninstall.sh; build the .deb
```

Guidelines:

- Keep the subject short and in the imperative mood ("add", "fix", "drop"),
  lowercase after the prefix, no trailing period.
- Use the body (after a blank line) for the *why* when it isn't obvious from
  the diff — the tradeoff you made, the bug you were chasing, the constraint
  you were honoring.
- One logical change per commit. Don't bundle a refactor with a feature.

---

## Code style

The overriding rule is **match the surrounding code.** Sluice values internal
consistency, so a change that reads like it was always there is better than
one that's individually clever.

- **Rust:** `rustfmt` is authoritative — don't hand-format. Keep clippy clean.
  Prefer clear names and small functions over comments that re-state the code;
  use comments for the non-obvious *why*, especially around the kernel ABI,
  the privilege boundary, and anything safety-related.
- **The shared types and the proto contract are load-bearing.** `sluice-types`
  and `sluice-proto` are consumed on both sides of the gRPC link. Changing the
  wire contract (`crates/sluice-proto/proto/sluice.proto`) or a shared value
  type ripples through both the engine and the UI — keep the two ends in sync
  in the same change, and treat the contract as something to evolve carefully.
- **Frontend:** the webview is dependency-free static HTML/CSS/JS by design
  (`crates/sluice-ui/frontend/`). Keep it that way — no framework, no build
  step, no bundler. Match the existing structure in `main.js`,
  `index.html`, and `style.css`. Note that some native webview controls
  (e.g. `<select>`) need explicit styling to render correctly in dark mode;
  follow the patterns already in `style.css`.
- **eBPF / engine:** the code under `engine/` is closer to the metal — it's
  `no_std` on the BPF side and deals with a `#[repr(C)]` ABI shared between
  kernel and userspace. Be conservative here, keep the kernel↔userspace
  struct layout in lockstep across `common`, `ebpf`, and `loader`, and lean on
  the existing comments that explain the ABI.

---

## Security & the privilege boundary

Sluice is a firewall. Mistakes in the wrong place can take the network down
or weaken protection, so a handful of invariants are non-negotiable and any
change near them gets extra scrutiny:

- **The UI never runs as root and never enforces.** Privileged work lives in
  the engine. The UI is an unprivileged gRPC client; it requests changes, the
  engine performs them.
- **The engine↔UI transport is hardened.** The engine serves on a Unix socket
  (`/run/sluice/engine.sock`) inside a `0700` directory, with a `0600` socket
  gated by an `SO_PEERCRED` check to the owner's uid (or root). Don't loosen
  those permissions or widen who can connect.
- **Fail safe, not open.** Never present a connection as "allowed" unless the
  rule write is actually confirmed. Ambiguity resolves to the more
  restrictive presentation.
- **Confirm anything that reduces protection.** Removing a block, broadening
  scope, or switching to a less-protective mode must be an explicit,
  surfaced-in-UI action — never a silent default.
- **No lockout.** Enforcement is default-allow plus a denylist, and a
  critical-host safelist refuses to block loopback/localhost. Don't introduce
  a path that could deny the user out of their own machine, and keep
  `sudo systemctl stop sluice-engine` as the always-available recovery.
- **Quiet by default.** Sluice does not phone home. Any network access must be
  opt-in and disclosed.

If your change touches enforcement, the transport, the rule store, or the
privilege line, say so prominently in the pull request and reference the
relevant part of [SECURITY.md](SECURITY.md).

---

## Reporting bugs

If something is broken, please open an issue. The more of the following you
can include, the faster it can be acted on:

- **What you tried to do** — the action, not just the symptom.
- **What happened instead** — the observed behavior, with a screenshot if it's
  UI-shaped.
- **Sluice version** — `cat VERSION` from the repo, or the version shown in the
  app.
- **Which half** — whether the issue is in the desktop UI, the engine, or the
  link between them. If the feed is empty or stale, check whether the engine is
  running: `systemctl status sluice-engine`.
- **Relevant logs** — for engine issues, the most useful 50–100 lines from
  `sudo journalctl -u sluice-engine -n 200 --no-pager`.
- **Environment** — your Ubuntu/GNOME version and kernel
  (`uname -r`), since the engine depends on cgroup-v2, eBPF, and `nftables`.

### Security-sensitive issues

If a bug is security-shaped — a way to cross the privilege boundary, escape
the socket's `SO_PEERCRED` gate, weaken enforcement, or cause a lockout —
**do not post exploit details in a public issue.** Follow the disclosure
process in [SECURITY.md](SECURITY.md) instead.

---

## Suggesting features

Feature ideas are welcome — open an issue and describe the problem you're
trying to solve, not just the solution you have in mind. A couple of realities
to set expectations:

- **The maintenance bar is high.** Sluice deliberately keeps a tight, coherent
  feature set with a clean privilege split. Proposals that fit that direction
  and respect the safety invariants are much more likely to land.
- **Anything that crosses the privilege boundary or adds network egress needs
  a strong justification.** Those are the parts of the design that are hardest
  to get right and easiest to weaken, so they get the most scrutiny.

Thanks for contributing.
