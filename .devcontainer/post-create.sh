#!/usr/bin/env bash
#
# Runs once, when the devcontainer is created. Everything here is idempotent,
# so re-running it by hand after a rebuild is safe.
#
# What it deliberately does NOT do is build the tree. A `--all-targets` dev
# build is ~9.2GB and several minutes, and most first contributions need one
# crate, not 79 binaries. `cargo fetch` below means the first real build is
# compile-bound rather than network-bound. (In Codespaces, a prebuild is the
# right place to warm the artifact cache — see .devcontainer/README.md.)

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

step() { printf '\n=== %s\n' "$1"; }

# --- Rust toolchain ----------------------------------------------------------
# The base image pins whatever stable was current when it was built. CI runs
# `dtolnay/rust-toolchain@stable`, i.e. *today's* stable, and newer clippy adds
# lints the older one does not emit — the toolchain gap documented in
# CLAUDE.md, where local `cargo lint` passes and CI fails with `-D warnings`.
# Pulling up to stable here closes it.
step "Rust toolchain"
rustup update stable
rustup default stable
rustup component add rustfmt clippy
rustc --version && cargo --version && cargo clippy --version

# --- Native build prerequisites ---------------------------------------------
# protoc: a transitive dependency builds proto files, so a plain workspace
# build needs it — not just `--features full`.
step "Native prerequisites"
scripts/setup-dev.sh

# --- Warm the cargo registry -------------------------------------------------
step "Fetching dependencies"
cargo fetch --locked

# --- Python bindings ---------------------------------------------------------
# Same layout the python-test workflow uses (a venv inside the crate), so the
# commands in CONTRIBUTING.md and in CI are the same commands. `maturin
# develop` itself is left to you: it compiles the extension module, which is
# minutes, and only matters if you are working on the bindings.
step "Python toolchain"
if command -v python3 >/dev/null 2>&1; then
    python3 -m venv crates/wingfoil-python/.venv
    crates/wingfoil-python/.venv/bin/pip install --quiet --upgrade pip maturin pytest
    echo "venv ready: crates/wingfoil-python/.venv (run 'maturin develop' inside it when you need the module)"
else
    echo "python3 not found, skipping the Python venv"
fi

cat <<'EOF'

=== Ready.

Check the tree builds and runs:

    cargo test -p wingfoil
    cargo run  -p wingfoil --example hello_graph

Before you push (these mirror CI):

    cargo fmt --all
    cargo lint          # default features
    cargo lint-all      # all features

Git hooks run fmt + clippy on commit and build + test on push, which is
minutes. For a docs-only or single-crate change:

    WINGFOIL_HOOKS=fast git commit ...   # fmt + a lib-only clippy
    WINGFOIL_HOOKS=off  git commit ...   # skip them; CI still gates the PR

Builds are large. `scripts/disk.sh` reports and reclaims space; `scripts/disk.sh
light` keeps target/*/deps so the next build relinks instead of recompiling.

Where to start: docs/wingfoil-architecture.md, then the `good first issue`
label. CONTRIBUTING.md has the full loop.
EOF
