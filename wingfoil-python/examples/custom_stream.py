#!/usr/bin/env python3

import math
from wingfoil import ticker, Stream


class MyStream(Stream):

    def cycle(self):
        self.value = 0
        for i, src in enumerate(self.sources()):
            self.value += src.peek_value() * math.pow(10, i)
            
period = 0.1 # seconds
source = ticker(period).count()
stream = MyStream([source]*3)
stream.run(
    realtime = True,
    cycles = 10,
)

