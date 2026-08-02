#!/usr/bin/env python3
"""Quick start — build a small wingfoil-next graph and run it.

A `Graph` holds the wiring; sources (`counter`, `constant`, `values`) and
combinators (`map`, `filter`, `distinct`, …) build streams on it; `run` drives
it and each stream's `value()` reads back its final value.

Build the module first, then run:
    cd crates/wingfoil-next-python && maturin develop
    python examples/quick_start.py
"""

import wingfoil_next as wf

g = wf.Graph()

greetings = (
    g.counter(period_nanos=100)  # ticks 1, 2, 3, … every 100ns
    .map(lambda n: f"hello world {n}")
    .inspect(print)  # side-effect tap: print each value as it flows
)

# Historical run (deterministic, no wall clock) for three ticks.
g.run(cycles=3)

print("final value:", greetings.value())
