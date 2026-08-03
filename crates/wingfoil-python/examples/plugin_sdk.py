#!/usr/bin/env python3
"""Plugin SDK — compose Rust-authored components from Python.

wingfoil lets you author ops, sub-graphs, and IO adapters in Rust and call
them from Python (the #[pyop] / #[pygraph] / #[pyadapter] macros). This module
ships a few demo components; here we wire them together with built-in
combinators — compiled-speed interiors, dynamic composition from Python.
"""

import wingfoil as wf

g = wf.Graph()

# ramp_source: a Rust *source adapter* (#[pyadapter]) -> 10, 12, 14, ...
ramp = wf.ramp_source(g, 10.0, 2.0)

# square: a Rust *op* (#[pyop]);  doubled_running_total: a Rust *sub-graph*
# (#[pygraph], double then cumulative-sum). Both fan out from `ramp`.
squared = wf.square(ramp)                 # 100, 144, 196, ...
totals = wf.doubled_running_total(ramp)   # 20, 44, 72, ...

# list_sink: a Rust *sink adapter* draining a stream into a Python list.
collected = []
wf.list_sink(squared, collected)

g.run(cycles=3)
print("squared (via Rust sink):", collected)
print("doubled running total  :", totals.value())
