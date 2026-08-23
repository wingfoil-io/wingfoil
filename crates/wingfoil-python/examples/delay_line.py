#!/usr/bin/env python3
"""Delay line — `delay()` re-emits each value later on the graph clock.

The delayed stream is a source in its own right: it ticks at `t + delay`, not
when its upstream ticks. `with_time()` pairs each value with the engine time it
arrived at, which is what makes the offset visible.

Ported from legacy wingfoil's `examples/delay_line.py`, which ran realtime and
slept for the delay; this runs historically, where the same schedule plays out
without a wall clock — the printed times are exact, not approximate.

    cd crates/wingfoil-python && maturin develop
    python examples/delay_line.py
"""

import wingfoil as wf

g = wf.Graph()

source = g.counter(period_nanos=100).inspect(
    lambda n: print(f"source ticked:  {n}")
)

# Delayed by a quarter of the source's period, so the two interleave.
source.delay(delay_nanos=25).with_time().inspect(
    lambda tv: print(f"delayed at t={tv[0]}: {tv[1]}")
)

g.run(cycles=8)
