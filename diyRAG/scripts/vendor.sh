#!/usr/bin/env bash
# diyRAG — produce a fully offline / air-gapped Cargo build.
# (MASTER_BUILD_SPEC.md §3.1 single static binaries, §12.9 supply-chain.)
#
# Run this ONCE on a machine WITH network access. It downloads the entire
# locked dependency set into ./vendor and writes .cargo/config.toml so that
# `cargo build --offline` (or --frozen) then builds with NO network — ideal for
# the local/LAN-only, air-gapped homelab/unraid deployment.
#
# Usage:
#   scripts/vendor.sh          # vendor all crates + write source replacement
#   cargo build --offline      # afterwards: builds with zero network
set -euo pipefail
cd "$(dirname "$0")/.."

[[ -f Cargo.lock ]] || cargo generate-lockfile
mkdir -p .cargo

echo "Vendoring the locked dependency set into ./vendor (downloads ~600 crates)…"
# `cargo vendor` prints the [source] replacement config on stdout; capture it.
cargo vendor --locked vendor > .cargo/config.toml

echo
echo "Done."
echo "  - ./vendor              vendored crate sources (gitignored; large)"
echo "  - .cargo/config.toml    source replacement (gitignored)"
echo "Now build with no network:  cargo build --offline   (or --frozen)"
echo "For a clean checkout, re-run this on a networked box, or ship ./vendor + .cargo/config.toml with the release."
