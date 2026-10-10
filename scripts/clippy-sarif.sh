#!/usr/bin/env bash
set -euo pipefail

# Keep Cargo's failure even when the report tools successfully render its errors.
cargo clippy --locked --workspace --all-targets --all-features --message-format=json -- -D warnings \
    | clippy-sarif \
    | tee rust-clippy-results.sarif \
    | sarif-fmt
