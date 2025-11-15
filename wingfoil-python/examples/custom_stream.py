#!/usr/bin/env python3

import math
from wingfoil import ticker, Stream


class MyStream(Stream):

    def cycle(self):
        value = 0
        for i, src in enumerate(self.upstreams()):
            value += src.peek_value() * math.pow(10, i)
        self.set_value(value)
        return True


period = 0.1 # seconds
source = ticker(period).count().logged("src")
(
    MyStream([source]*3)
        .map(lambda x : x * 0.01)
        .logged("out")
        .run(
            realtime = True,
            cycles = 5,
        )
)

