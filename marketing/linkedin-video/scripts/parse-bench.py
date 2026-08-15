#!/usr/bin/env python3
"""Turn criterion's estimates into the performance figures scene 3 renders.

Reads `target/criterion/<group>/<tier>/new/` — criterion's own structured
output, not its stdout — so the numbers on screen are the ones the harness
measured, on the machine that measured them.

Only wingfoil's own execution tiers are recorded. The bench also runs a
`legacy` bar, which is an internal regression baseline rather than anything to
put in front of an audience, and no third-party comparison is captured at all:
a ratio against another library is only meaningful with its workload attached,
and a marketing frame is the wrong place to carry that much fine print.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# The tiers worth showing, in the order a reader should meet them.
TIERS = ["interpreted", "nested", "compiled"]


def read(crit: Path, group: str, tier: str) -> tuple[float, int]:
    base = crit / group / tier / "new"
    est = json.loads((base / "estimates.json").read_text())
    bench = json.loads((base / "benchmark.json").read_text())
    ns_per_iter = est["mean"]["point_estimate"]
    elements = bench["throughput"]["Elements"]
    return ns_per_iter, elements


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--criterion", required=True, help="target/criterion")
    ap.add_argument("--group", default="fanout")
    ap.add_argument("--nodes", type=int, required=True)
    ap.add_argument("--cycles", type=int, required=True)
    ap.add_argument("--out", required=True)
    args = ap.parse_args()

    crit = Path(args.criterion)
    tiers = {}
    for tier in TIERS:
        ns_per_iter, elements = read(crit, args.group, tier)
        expected = args.nodes * args.cycles
        if elements != expected:
            raise SystemExit(
                f"{tier}: criterion counted {elements} elements, expected "
                f"{args.nodes} nodes x {args.cycles} cycles = {expected}. "
                "The bench's node count changed — update --nodes."
            )
        tiers[tier] = {
            "nsPerIteration": round(ns_per_iter, 1),
            "nsPerCycle": round(ns_per_iter / args.cycles, 2),
            "nsPerNodeCycle": round(ns_per_iter / elements, 4),
        }

    slowest, fastest = tiers["interpreted"], tiers["compiled"]
    speedup = slowest["nsPerCycle"] / fastest["nsPerCycle"]

    out = {
        "note": (
            "Measured by scripts/capture-bench.sh on the machine that rendered "
            "the video, from crates/wingfoil/benches/tiers.rs. Absolute times "
            "are machine-specific; re-run on a quiet host to refresh."
        ),
        "workload": args.group,
        "nodes": args.nodes,
        "cycles": args.cycles,
        "tiers": tiers,
        "compiledSpeedup": round(speedup, 1),
    }
    Path(args.out).write_text(json.dumps(out, indent=2) + "\n")

    print(f"workload {args.group}: {args.nodes} nodes x {args.cycles} cycles")
    for tier in TIERS:
        t = tiers[tier]
        print(f"  {tier:<12} {t['nsPerCycle']:>10.2f} ns/cycle  {t['nsPerNodeCycle']:>7.3f} ns/node-cycle")
    print(f"  compiled vs interpreted: {speedup:.1f}x")


if __name__ == "__main__":
    main()
