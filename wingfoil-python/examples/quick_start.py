#!/usr/bin/env python3

from wingfoil import ticker
import wingfoil

print("wingfoil.__version__=%s" % wingfoil.__version__)

period = 0.1 # seconds

stream = (
    ticker(period)
        .count()
        .map(lambda x: f"hello world {x}")
        .logged(">>")
)

stream.run(
    realtime = True,
    cycles = 3,
)

print(stream.peek_value())

