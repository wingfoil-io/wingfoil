#!/usr/bin/env bash
# Report and reclaim build-artifact disk usage.
#
#   scripts/disk.sh            # report only — what is using space
#   scripts/disk.sh light      # drop the cheap-to-rebuild bulk (examples, benches,
#                              #   incremental, extracted registry sources)
#   scripts/disk.sh deep       # light + every target/ dir in the tree
#   scripts/disk.sh auto       # light (then deep) only if headroom is low
#
# `light` is the one to reach for mid-session: it keeps target/*/deps, so the
# next `cargo build` relinks rather than recompiling 700+ crates. The example
# and bench binaries it deletes are the bulk of the tree (each one statically
# links the whole dependency graph) and nothing but `--all-targets` needs them.
#
# `auto` is the unattended form, for hooks: it costs one `df` and stays silent
# unless it actually reclaimed something. Thresholds are in GB and tunable:
#
#   WINGFOIL_DISK_MIN_GB=10    # below this, run `light`
#   WINGFOIL_DISK_FLOOR_GB=4   # still below this afterwards, escalate to `deep`
#
# The default of 10GB is sized off the build it has to leave room for: a
# `--all-targets` dev build of this workspace lands around 9.2GB.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-report}"

human() { du -sh "$@" 2>/dev/null | sort -hr; }
avail() { df -h "$root" | awk 'NR==2 {print $4}'; }
# Whole GB of headroom, for arithmetic. `df -Pk` rather than `--output=avail`
# so this keeps working on macOS.
avail_gb() { df -Pk "$root" | awk 'NR==2 {print int($4/1048576)}'; }

# target/ dirs anywhere in the tree: the workspace root, plus wingfoil-wasm,
# which is excluded from that workspace (different target triple) and so
# builds into its own.
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
          "$root"/js/node_modules 2>/dev/null
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

# Silent no-op above the threshold; one line when it reclaimed something. That
# silence is the point — this runs after every shell command in a Claude Code
# session (see .claude/settings.json), and a hook that narrates every tick is a
# hook people turn off.
auto() {
    local min="${WINGFOIL_DISK_MIN_GB:-10}"
    local floor="${WINGFOIL_DISK_FLOOR_GB:-4}"
    local before after
    before="$(avail_gb)"
    if [ "$before" -ge "$min" ]; then
        return 0
    fi

    light >/dev/null
    after="$(avail_gb)"
    if [ "$after" -ge "$floor" ]; then
        echo "disk: ${before}GB free (< ${min}GB) — ran \`scripts/disk.sh light\`," \
             "dropping example/bench binaries, incremental and extracted registry sources." \
             "Now ${after}GB free; target/*/deps kept, so the next build relinks."
        return 0
    fi

    # Light left us under the floor, so the deps/ it was protecting are all
    # that is left to give. A full rebuild beats a build that dies halfway
    # through with ENOSPC.
    deep >/dev/null
    echo "disk: ${before}GB free (< ${min}GB), still ${after}GB after a light clean" \
         "— ran \`scripts/disk.sh deep\`; every target/ is gone, so the next build is a full one." \
         "Now $(avail_gb)GB free."
}

# `auto`, wrapped for a Claude Code PostToolUse hook (.claude/settings.json).
# The envelope is what puts the reclaim into the model's context — a hook that
# only writes to stdout tells the user but leaves the agent believing its
# example binaries are still there.
hook() {
    local out
    out="$(auto 2>&1)" || return 0
    [ -n "$out" ] || return 0
    if command -v jq >/dev/null 2>&1; then
        jq -cn --arg m "$out" \
            '{systemMessage: $m,
              hookSpecificOutput: {hookEventName: "PostToolUse", additionalContext: $m}}'
    else
        # No jq: the user still gets told, the model just misses the context.
        printf '%s\n' "$out"
    fi
}

case "$mode" in
    report) report ;;
    light)  light; echo; report ;;
    deep)   deep; echo; report ;;
    auto)   auto ;;
    hook)   hook ;;
    *) echo "usage: $(basename "$0") [report|light|deep|auto|hook]" >&2; exit 1 ;;
esac
