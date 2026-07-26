#!/usr/bin/env python3
"""Custom node — a Python object acting as a graph node.

`Graph.custom_node(upstreams, obj)` wires a Python object into the graph as a
first-class node. The object implements the node protocol:
  - cycle(values) -> bool : given the upstreams' current values, did it tick?
  - peek() -> value       : its output value when it ticked.
"""

import wingfoil_next as wf


class RunningMax:
    """Emits the running maximum of its single upstream."""

    def __init__(self):
        self.hi = None

    def cycle(self, values):
        (v,) = values
        self.hi = v if self.hi is None else max(self.hi, v)
        return True

    def peek(self):
        return self.hi


g = wf.Graph()

# A bouncy input: (n * 7) % 5  ->  2, 4, 1, 3, 0, 2
source = g.counter(period_nanos=100).map(lambda n: (n * 7) % 5)

running_max = g.custom_node([source], RunningMax()).inspect(
    lambda v: print("max so far:", v)
)

g.run(cycles=6)
print("final max:", running_max.value())
