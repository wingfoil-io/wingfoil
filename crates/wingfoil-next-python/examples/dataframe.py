#!/usr/bin/env python3
"""DataFrame — collect one stream into a pandas DataFrame, and join several.

`stream.dataframe()` accumulates each value with its engine time and, on the
last cycle, produces a pandas DataFrame (columns `time`, `value`) as the
stream's final value.

`build_dataframe({name: stream, ...})` is the multi-stream half: it outer-joins
several already-run streams on their engine time into one frame, one column per
key. A stream holds its history either as a frame (`dataframe()`) or as
`(time, value)` tuples (`collect()`); both are accepted.

Requires pandas (`pip install pandas`).
"""

import wingfoil_next as wf

print("~~~ Single stream (primitives) ~~~")

g = wf.Graph()
df = g.counter(period_nanos=100).map(lambda n: n * n).dataframe()  # squares
g.run(cycles=5)

print(df.value())

print("\n~~~ Single stream (dictionaries) ~~~")

g = wf.Graph()
quotes = (
    g.counter(period_nanos=100)
    .map(lambda i: {"price": 100 + i, "qty": 10})
    .dataframe()
)
g.run(cycles=5)

print(quotes.value())

print("\n~~~ Multiple streams (build_dataframe) ~~~")

g = wf.Graph()
source = g.counter(period_nanos=100)
price = source.map(lambda i: 100 + i).dataframe()
qty = source.map(lambda _: 10).dataframe()
g.run(cycles=5)

print(wf.build_dataframe({"price": price, "qty": qty}))

print("\n~~~ Streams at different rates (outer join fills NaN) ~~~")

# A slower stream is quiet on the run's last cycle, so it never reaches
# `dataframe()`'s build step — `collect()` is the shape to join on here.
g = wf.Graph()
fast = g.counter(period_nanos=100).map(lambda n: n * 10).collect()
slow = g.counter(period_nanos=200).map(lambda n: n * 100).collect()
g.run(cycles=4)

print(wf.build_dataframe({"fast": fast, "slow": slow}))
