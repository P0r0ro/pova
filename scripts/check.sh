#!/usr/bin/env bash
# Quality gate run before every milestone commit: formatting, clippy with
# warnings as errors, and the offline test suite.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
