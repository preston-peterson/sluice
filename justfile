# Sluice common tasks. Run `just` to list. `just setup` installs `just` itself + the toolchain; if
# you don't have it yet, every recipe's body is plain cargo/sh and works standalone.
#
# Two workspaces: the root (crates/*: sluice-ui + sluice-proto + shared types, pinned stable) and the
# detached engine (engine/*: the eBPF program + the root loader, nightly + bpf-linker).

# Show available recipes.
default:
    @just --list

# Install dev deps (Rust + protoc + Tauri libs + just) and verify prerequisites. Idempotent.
setup:
    ./scripts/setup.sh

# Format, lint, and test the root workspace — the CI gate.
check:
    cargo fmt --all -- --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace

# Supply-chain audit: scan dependencies against the RUSTSEC advisory DB (needs `cargo install cargo-audit`).
audit:
    cargo audit

# Auto-format the tree.
fmt:
    cargo fmt --all

# Run the desktop app (Tauri) against a running engine. Needs the Tauri system libs (just setup).
ui:
    cargo run -p sluice-ui

# Build the Sluice engine: the eBPF object (nightly) + the host loader.
engine-build:
    cd engine/ebpf && cargo build --release
    cd engine/loader && cargo build --release

# Install the engine as a root systemd service (after engine-build). Disables conflicting firewalls.
engine-install: engine-build
    sudo engine/install.sh

# Build the combined .deb (UI + the prebuilt engine) under target/release/bundle/deb/. Needs tauri-cli.
package: engine-build
    mkdir -p crates/sluice-ui/dist-engine
    install -m 0755 engine/loader/target/release/sluice-engine                crates/sluice-ui/dist-engine/sluice-engine
    install -m 0644 engine/ebpf/target/bpfel-unknown-none/release/sluice-ebpf crates/sluice-ui/dist-engine/sluice-ebpf
    install -m 0644 engine/sluice-engine.service                              crates/sluice-ui/dist-engine/sluice-engine.service
    cd crates/sluice-ui && PROTOC="${PROTOC:-$HOME/.local/bin/protoc}" cargo tauri build --bundles deb

# Fetch the offline geoIP database (DB-IP Lite, CC-BY-4.0) for country lookups. Per-machine; not committed.
geoip:
    bash scripts/fetch-geoip.sh

# Release (option C): write notes under "## [Unreleased]" in CHANGELOG.md, then run these three:
#   just release-prep X.Y.Z  ·  git push origin main vX.Y.Z  ·  just sign-release X.Y.Z

# Bump version + stamp CHANGELOG + commit + tag a release (then push the tag to build/draft it).
release-prep VERSION:
    bash scripts/release-prep.sh {{VERSION}}

# Sign the CI-built draft release with the offline key, then publish it.
sign-release VERSION:
    bash scripts/sign-release.sh {{VERSION}}

# All-local fallback (build + sign + --publish on this machine). Prefer release-prep/sign-release.
release *ARGS:
    bash scripts/package-release.sh {{ARGS}}
