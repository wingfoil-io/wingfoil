#!/usr/bin/env python3
"""Combine two streams running at different rates.

`bimap` reads both inputs and ticks when either does, taking the other's
current held value. `sample` is the opposite pairing: it holds a value and
re-emits it on someone else's tick — here turning a constant into something
that ticks.

Ported from legacy wingfoil's `examples/examples.py` (renamed for what it
shows), where `bimap` was a free function over two streams; here it is a method
on the first of them.

    cd crates/wingfoil-python && maturin develop
    python examples/combine.py
"""

import wingfoil as wf

g = wf.Graph()

slow = g.counter(period_nanos=200)  # 1, 2, 3, … every 200ns
fast = g.constant(0.7).sample(g.counter(period_nanos=100))  # 0.7 every 100ns

total = slow.bimap(fast, lambda a, b: a + b).with_time()
total.inspect(lambda tv: print(f"t={tv[0]:>4}  {tv[1]}"))

g.run(cycles=10)

print("final value:", total.value())
