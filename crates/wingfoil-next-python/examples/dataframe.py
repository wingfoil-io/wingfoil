#!/usr/bin/env python3
"""DataFrame — collect a stream into a pandas DataFrame.

`stream.dataframe()` accumulates each value with its engine time and, on the
last cycle, produces a pandas DataFrame (columns `time`, `value`) as the
stream's final value. Requires pandas (`pip install pandas`).
"""

import wingfoil_next as wf

g = wf.Graph()

df = g.counter(period_nanos=100).map(lambda n: n * n).dataframe()  # squares
g.run(cycles=5)

print(df.value())
