#!/usr/bin/env python3
"""Custom stream — the subclass form, ported from legacy wingfoil.

The twin of `custom_stream.py`: same capability, legacy's shape. Subclass
`CustomStream`, implement `cycle()`, read the upstreams off `self`. The
constructor returns the wired `Stream`, so it chains like any other.

This is legacy's `examples/custom_stream.py`, changed only where next requires
it: the graph is passed explicitly (next has no ambient graph) and `upstreams()`
yields this cycle's values rather than the upstream Stream objects.
"""

import math

import wingfoil as wf


class MyStream(wf.CustomStream):
    """Combines its upstreams as digits of a base-10 number."""

    def cycle(self):
        value = 0
        for i, src in enumerate(self.upstreams()):
            value += src.peek_value() * math.pow(10, i)
        self.set_value(value)
        return True


g = wf.Graph()

source = g.counter(period_nanos=100)

# The same source wired three times: n + 10n + 100n == 111n, scaled to 1.11n.
out = MyStream(g, [source] * 3).map(lambda x: x * 0.01)
out.inspect(lambda v: print("out:", v))

g.run(cycles=5)
print("final:", out.value())
