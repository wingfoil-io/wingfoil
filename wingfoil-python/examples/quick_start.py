#!/usr/bin/env python3

from wingfoil import ticker
import wingfoil

print("wingfoil.__version__=%s" % wingfoil.__version__)

period = 1.0 # seconds


src = ticker(period).count().logged("hello, world")
src.run(
    realtime = True,
    duration = 3,
)

print(src.peek_value())

