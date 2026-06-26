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

# Build an installable .deb (release build + Tauri bundler). Output under
# target/release/bundle/deb/. Needs the Tauri CLI: `cargo install tauri-cli` (or `just setup`).
package:
    cd crates/sluice-ui && PROTOC="${PROTOC:-$HOME/.local/bin/protoc}" cargo tauri build --bundles deb

# Build the Sluice engine (eBPF object on nightly + the host loader). See engine/README.md.
engine-build:
    cd engine/ebpf && cargo build --release
    cd engine/loader && cargo build --release

# Install the engine as a root systemd service (after engine-build). Disables conflicting firewalls.
engine-install: engine-build
    sudo engine/install.sh

# Fetch the offline geoIP database (DB-IP Lite, CC-BY-4.0) for country lookups in the connection
# detail (FR-052). Per-machine; not committed. Sluice reads it locally — no network at runtime.
geoip:
    bash scripts/fetch-geoip.sh
