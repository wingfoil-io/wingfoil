#!/usr/bin/env bash
# Capture the terminal frames used by scenes 4 and 5 by actually running the
# committed `top_of_book` example, once per run mode. Nothing in the video's
# terminal is hand-written: `assets/terminal.json` is regenerated from real
# output, and it records the command that produced it so the on-screen prompt
# line is captured too rather than typed.
#
# The realtime pass replays three seconds of market time at its original pace,
# so it takes about three seconds to run. That is the point of it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
repo="$(cd "$root/../.." && pwd)"
out="$root/assets/terminal.json"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# The command as a viewer would type it from a clone of the repo.
display="cargo run --example top_of_book"

run() {
  local mode="$1"
  local args=()
  [ "$mode" = "realtime" ] && args=(-- realtime)
  cargo run -q \
    --manifest-path "$repo/crates/wingfoil/Cargo.toml" \
    --example top_of_book "${args[@]}" 2>&1
}

echo "capturing historical run…" >&2
run historical > "$tmp/historical.txt"
echo "capturing realtime run…" >&2
run realtime > "$tmp/realtime.txt"

python3 "$here/parse-capture.py" \
  --historical "$tmp/historical.txt" \
  --realtime "$tmp/realtime.txt" \
  --command "$display" \
  --out "$out"
