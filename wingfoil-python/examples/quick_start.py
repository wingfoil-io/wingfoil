#!/usr/bin/env python3

from wingfoil import ticker

period = 1.0 # seconds

src = ticker(period).count().logged("hello, world")
src.run()
print(src.peek_value())