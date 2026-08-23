#!/usr/bin/env python3
"""Deduplicate — `distinct()` drops consecutive repeats.

A stream that repeats each value three times becomes 0, 1, 2, … The filter is
on *consecutive* equality, not on everything seen before: a value that comes
back after something else ticks through will tick again.

Ported from legacy wingfoil's `examples/deduplicate.py`, which ran realtime;
this runs historically, so the output is the same every time and finishes
instantly.

    cd crates/wingfoil-python && maturin develop
    python examples/deduplicate.py
"""

import wingfoil as wf

g = wf.Graph()

clean = (
    g.counter(period_nanos=100)  # 1, 2, 3, …
    .map(lambda n: (n - 1) // 3)  # 0, 0, 0, 1, 1, 1, 2, …
    .inspect(lambda v: print(f"raw:      {v}"))
    .distinct()
    .inspect(lambda v: print(f"distinct: {v}"))
)

g.run(cycles=10)

print("final value:", clean.value())
