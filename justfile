# Sluice common tasks. Run `just` to list. Requires `just` (https://github.com/casey/just);
# if you don't have it, the underlying commands are plain cargo/sh and work standalone.
#
# Two workspaces: the root (crates/*: sluice-ui + sluice-proto + the shared engine types) and the
# detached engine (engine/*: the eBPF program + the root loader). The engine needs nightly +
# bpf-linker for its eBPF crate (see engine/README.md); the root builds on the pinned stable.

# Show available recipes.
default:
    @just --list

# Install dev deps (Rust + protoc) and verify prerequisites. Idempotent.
setup:
    ./scripts/setup.sh

# Format, lint, and test the root workspace. The CI gate.
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Supply-chain audit: scan dependencies against the RUSTSEC advisory DB (SEC-010). Kept separate
# from `check`. Needs `cargo install cargo-audit`.
audit:
    cargo audit

# Auto-format the tree.
fmt:
    cargo fmt --all

# Run the Sluice desktop app (Tauri). It connects to the Sluice engine over UDS (engine mode is
# the default; SLUICE_ENGINE_UDS overrides the socket). Needs the Tauri system libs (just setup).
ui:
    cargo run -p sluice-ui

# Build the combined installable .deb (UI + the prebuilt engine, staged from engine-build).
# Output under target/release/bundle/deb/. Needs the Tauri CLI: `cargo install tauri-cli`.
package: engine-build
    mkdir -p crates/sluice-ui/dist-engine
    install -m 0755 engine/loader/target/release/sluice-engine                crates/sluice-ui/dist-engine/sluice-engine
    install -m 0644 engine/ebpf/target/bpfel-unknown-none/release/sluice-ebpf crates/sluice-ui/dist-engine/sluice-ebpf
    install -m 0644 engine/sluice-engine.service                              crates/sluice-ui/dist-engine/sluice-engine.service
    cd crates/sluice-ui && PROTOC="${PROTOC:-$HOME/.local/bin/protoc}" cargo tauri build --bundles deb

# Build the Sluice engine (eBPF object on nightly + the host loader). See engine/README.md.
engine-build:
    cd engine/ebpf && cargo build --release
    cd engine/loader && cargo build --release

# Install the engine as a root systemd service (after engine-build). Disables conflicting firewalls.
engine-install: engine-build
    sudo engine/install.sh

# Fetch the offline geoIP database (DB-IP Lite, CC-BY-4.0) for country lookups in the connection
# detail. Per-machine; not committed. Sluice reads it locally — no network at runtime.
geoip:
    bash scripts/fetch-geoip.sh

# Release (option C — the normal path): CI builds + drafts, you sign + publish locally.
#   1) write notes under "## [Unreleased]" in CHANGELOG.md
#   2) just release-prep X.Y.Z      # bump version + stamp CHANGELOG + commit + tag
#   3) git push origin main vX.Y.Z  # the Release workflow builds + drafts
#   4) just sign-release X.Y.Z      # sign locally with the offline key + publish
release-prep VERSION:
    bash scripts/release-prep.sh {{VERSION}}

sign-release VERSION:
    bash scripts/sign-release.sh {{VERSION}}

# All-local fallback: build + sign + (with --publish) cut a release entirely on this machine.
# Use when CI is unavailable; otherwise prefer the release-prep / sign-release flow above.
release *ARGS:
    bash scripts/package-release.sh {{ARGS}}
