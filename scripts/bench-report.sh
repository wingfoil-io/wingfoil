#!/usr/bin/env bash
# Run the wingfoil-next benchmark suite and refresh the captured report.
#
#   scripts/bench-report.sh              # run everything, refresh images/, print estimates
#   scripts/bench-report.sh --no-run     # just re-harvest plots/estimates from the last run
#
# Runs every bench target that needs no external service or system dependency
# (so: not the aeron/iceoryx2 ones), copies criterion's plots into
# crates/wingfoil-next/benches/images/, and prints each benchmark's point
# estimate so the tables in benches/README.md can be refilled by hand.
#
# The two hand-drawn charts are NOT regenerated here — they carry their data
# inline, so paste the numbers this prints into them and re-run:
#
#   crates/wingfoil-next/benches/plot_tiers.py       (tier summary)
#   crates/wingfoil-next/benches/topological_vs_per_path/plot.py  (topological
#                                                    sort vs per-path propagation)
#
# Benchmarks want a quiet machine: nothing else running, and ideally not a
# shared cloud VM. They are not a CI gate and are not meant to become one (see
# benches/README.md); the deterministic perf gates are tests.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crit="$root/target/criterion"
img="$root/crates/wingfoil-next/benches/images"

# One feature set across every target, so all the numbers come from a single
# consistently-built artifact set (and cargo does not rebuild between them).
features="bench,async"

targets=(
  graph
  nanotime
  tiers
  custom_op
  store_baseline
  bfs_vs_dfs_wingfoil
  bfs_vs_dfs_reactive
  bfs_vs_dfs_async_streams
)

if [[ "${1:-}" != "--no-run" ]]; then
  for target in "${targets[@]}"; do
    echo "==> $target"
    cargo bench -p wingfoil-next --features "$features" --bench "$target"
  done
fi

echo "==> harvesting plots into ${img#"$root"/}"
mkdir -p "$img/graph" "$img/tiers" "$img/ops"

# Headline graph bench: the same plot set legacy captured for its 10x10 run.
for plot in pdf regression typical mean median SD MAD slope; do
  cp "$crit/10x10/report/$plot.svg" "$img/graph/10x10_$plot.svg"
done
cp "$crit/node/report/pdf.svg" "$img/graph/node_pdf.svg"
cp "$crit/100x100/report/pdf.svg" "$img/graph/100x100_pdf.svg"
cp "$crit/nanotime/report/pdf.svg" "$img/graph/nanotime_pdf.svg"

# One violin per tier workload, plus the two next-only op suites.
for group in dense_chain fanout fan_in_16 fan_in_64 fan_in_256 \
             accumulate sparse sparse_wide; do
  cp "$crit/$group/report/violin.svg" "$img/tiers/$group.svg"
done
cp "$crit/dense_chain_20/report/violin.svg" "$img/ops/custom_op_dense_chain_20.svg"
cp "$crit/sparse_dispatch/report/violin.svg" "$img/ops/store_sparse_dispatch.svg"
cp "$crit/forward_clone/report/violin.svg" "$img/ops/store_forward_clone.svg"

command -v lscpu >/dev/null && lscpu > "$img/lscpu.txt"

echo
echo "==> estimates"
# Note: the three bfs_vs_dfs targets all name their benchmarks depth_1..depth_10,
# so target/criterion holds only whichever ran last. Take those three series off
# the console output above, not from here.
python3 - "$crit" <<'PY'
import json, os, sys

root = sys.argv[1]


def fmt(ns):
    for scale, unit in ((1e9, "s"), (1e6, "ms"), (1e3, "µs")):
        if ns >= scale:
            return f"{ns / scale:.4g} {unit}"
    return f"{ns:.4g} ns"


rows = []
for dirpath, _, filenames in os.walk(root):
    if os.path.basename(dirpath) != "new" or "estimates.json" not in filenames:
        continue
    name = os.path.relpath(os.path.dirname(dirpath), root)
    with open(os.path.join(dirpath, "estimates.json")) as f:
        est = json.load(f)
    point = (est.get("slope") or est["mean"])["point_estimate"]
    thrpt = ""
    bench = os.path.join(dirpath, "benchmark.json")
    if os.path.exists(bench):
        with open(bench) as f:
            elems = (json.load(f).get("throughput") or {}).get("Elements")
        if elems:
            thrpt = f"  {elems / (point / 1e9) / 1e6:.1f} Melem/s"
    rows.append((name, point, thrpt))

for name, point, thrpt in sorted(rows):
    print(f"{name:<45} {fmt(point):>12}{thrpt}")
PY
