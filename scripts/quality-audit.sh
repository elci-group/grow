#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

echo "== Rust checks =="
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

echo "== Architecture =="
fract -c fract.toml index

echo "== Dependency policy =="
# Amber currently requires a non-virtual Cargo root. Keep this explicit until
# its workspace support lands; the command is intentionally fail-fast.
amber "$repo_root" --config "$repo_root/.amber.toml"

echo "== Release readiness =="
kaptaind-cli ci-hint --format json
