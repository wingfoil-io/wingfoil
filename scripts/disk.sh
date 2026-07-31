#!/usr/bin/env bash
# Report and reclaim build-artifact disk usage.
#
#   scripts/disk.sh            # report only — what is using space
#   scripts/disk.sh light      # drop the cheap-to-rebuild bulk (examples, benches,
#                              #   incremental, extracted registry sources)
#   scripts/disk.sh deep       # light + every target/ dir in the tree
#
# `light` is the one to reach for mid-session: it keeps target/*/deps, so the
# next `cargo build` relinks rather than recompiling 700+ crates. The example
# and bench binaries it deletes are the bulk of the tree (each one statically
# links the whole dependency graph) and nothing but `--all-targets` needs them.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-report}"

human() { du -sh "$@" 2>/dev/null | sort -hr; }
avail() { df -h "$root" | awk 'NR==2 {print $4}'; }

# target/ dirs anywhere in the tree: the workspace root, plus wingfoil-wasm,
# which is excluded from the workspace and so builds into its own.
targets() { find "$root" -type d -name target -prune 2>/dev/null; }

report() {
    echo "== available =="
    avail
    echo
    echo "== target dirs =="
    while IFS= read -r t; do human "$t"; done < <(targets)
    echo
    echo "== inside the workspace target =="
    human "$root"/target/*/deps "$root"/target/*/examples \
          "$root"/target/*/incremental "$root"/target/*/build 2>/dev/null | head -20
    echo
    echo "== caches outside the repo =="
    human "${CARGO_HOME:-$HOME/.cargo}/registry/src" \
          "${CARGO_HOME:-$HOME/.cargo}/registry/cache" \
          "$root"/wingfoil-js/node_modules "$root"/wingfoil-python/.venv 2>/dev/null
}

light() {
    while IFS= read -r t; do
        # examples/ and benches/ hold the statically-linked binaries; incremental/
        # is per-edit scratch that a clean rebuild regenerates.
        rm -rf "$t"/*/examples "$t"/*/benches "$t"/*/incremental
    done < <(targets)
    # Extracted crate sources — cargo re-expands these from registry/cache on
    # demand, so deleting them costs unpacking time, not a re-download.
    rm -rf "${CARGO_HOME:-$HOME/.cargo}/registry/src"
    echo "light clean done — available: $(avail)"
}

deep() {
    light
    while IFS= read -r t; do rm -rf "$t"; done < <(targets)
    echo "deep clean done — available: $(avail)"
}

case "$mode" in
    report) report ;;
    light)  light; echo; report ;;
    deep)   deep; echo; report ;;
    *) echo "usage: $(basename "$0") [report|light|deep]" >&2; exit 1 ;;
esac
