#!/usr/bin/env bash
# Measure the execution tiers scene 3 quotes, by running the committed
# benchmark. Like the terminal capture, nothing here is typed by hand:
# `assets/bench.json` is regenerated from criterion's own output.
#
# The workload is `fanout` from crates/wingfoil/benches/tiers.rs — a ticker and
# counter fanning out to 10x10 maps and recombining through a 10-way merge,
# which is the same fan-out-and-recombine shape scene 3 animates.
#
# Benchmarks want a quiet machine. On a shared cloud VM the absolute times run
# high and the interpreted tier suffers most, which flatters the ratio. Re-run
# this on a quiet host before treating the numbers as anything but indicative.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/.." && pwd)"
repo="$(cd "$root/../.." && pwd)"

# Must match the tier_group! invocation in benches/tiers.rs — the parser fails
# loudly if criterion's element count disagrees.
group="fanout"
nodes=103
cycles=10000

if [ "${1:-}" != "--no-run" ]; then
  echo "running the $group tier benchmark (a few minutes)…" >&2
  cargo bench --manifest-path "$repo/crates/wingfoil/Cargo.toml" \
    --features bench,async --bench tiers -- "$group" >&2
fi

python3 "$here/parse-bench.py" \
  --criterion "$repo/target/criterion" \
  --group "$group" \
  --nodes "$nodes" \
  --cycles "$cycles" \
  --out "$root/assets/bench.json"
