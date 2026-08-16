#!/usr/bin/env python3
"""Latency — stamp a payload through a pipeline and report the per-hop deltas.

A `Latency` record declares the stage names; a `TracedBytes` carries a payload
plus one such record. `stamp` writes the current wall-clock reading into the
named slot as the value passes, and `latency_report` aggregates the deltas
between consecutive stages over the whole run.

Two differences from legacy wingfoil's `examples/latency.py`:

- the latency ops are **free functions** (`wf.stamp(stream, "encode")`), not
  stream methods, so they chain by nesting or by rebinding as below;
- `latency_report` returns `(sink, stats)`, so the numbers are readable from
  Python rather than only printable at teardown.

**`stamp` vs `stamp_precise`, and why this example uses the precise one.** The
engine reads the wall clock at most **once per cycle** and caches it, so plain
`stamp` is nearly free — but every stamp in the same cycle reads the *same*
instant, and a chain of them reports hops of 0ns. `stamp_precise` takes a fresh
reading each time, which is what makes an in-cycle pipeline measurable. Mixing
them backwards is worse than useless: a plain `stamp` after a precise one reads
*earlier* than the stamp before it, and the report drops the hop as negative
rather than counting it. Use plain `stamp` to time hops that cross cycles or
processes — a network edge, a delay — and `stamp_precise` within one cycle.

The wall-clock reading is real in either run mode, so a historical run still
measures real time — which is why this needs no realtime loop.

    cd crates/wingfoil-python && maturin develop
    python examples/latency.py
"""

import wingfoil as wf

STAGES = ["produce", "encode", "decode", "strategy", "ack"]

STAMP = True  # flip to False to wire the same graph with stamping disabled


def make_payload(seq):
    """Wrap a sequence number in TracedBytes with an empty latency record."""
    return wf.TracedBytes(f"quote-{seq}".encode(), wf.Latency(STAGES))


g = wf.Graph()

pipeline = g.counter(period_nanos=10_000).map(make_payload)
pipeline = wf.stamp_precise_if(pipeline, "produce", STAMP)
pipeline = wf.stamp_precise_if(pipeline, "encode", STAMP)

# ── pretend strategy work happens here ──
pipeline = pipeline.map(lambda traced: traced)

pipeline = wf.stamp_precise_if(pipeline, "decode", STAMP)
pipeline = wf.stamp_precise_if(pipeline, "strategy", STAMP)
pipeline = wf.stamp_precise_if(pipeline, "ack", STAMP)

_sink, stats = wf.latency_report_if(pipeline, STAGES, STAMP, output="stdout")

print("Running latency pipeline for 500 cycles...")
g.run(cycles=500)

# The report prints itself at teardown; the same numbers are readable here.
# There is one hop *between* stages, so the first stage has no entry.
if STAMP:
    for stage in STAGES[1:]:
        hop = stats[stage]
        print(
            f"{stage:>9}: p50 {hop['p50_ns']}ns  p99 {hop['p99_ns']}ns  n={hop['count']}"
        )
