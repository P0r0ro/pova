#!/usr/bin/env bash
# Quality gate: formatting, clippy with warnings as errors, and the offline
# test suite. Run it before every commit; CI runs exactly this script too.
# --locked: fail if Cargo.lock is out of date instead of silently updating it.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
