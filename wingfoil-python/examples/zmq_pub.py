#!/usr/bin/env python3
"""ZMQ publisher example: publishes a counter as bytes every 500ms."""

import struct
import wingfoil as wf

PORT = 5555
print(f"Publishing on tcp://127.0.0.1:{PORT} ...")

(
    wf.ticker(0.5)
    .count()
    .inspect(lambda n: print(f"publishing: {n}"))
    .map(lambda n: struct.pack(">Q", n))  # u64 big-endian bytes
    .zmq_pub(PORT)
    .run(realtime=True)
)
