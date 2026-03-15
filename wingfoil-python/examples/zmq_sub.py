#!/usr/bin/env python3
"""ZMQ subscriber example: receives and prints data + connection status."""

import struct
import wingfoil as wf

ADDRESS = "tcp://127.0.0.1:5555"
print(f"Connecting to {ADDRESS} ...")

data, status = wf.py_zmq_sub(ADDRESS)

data_node = (
    data
    .inspect(lambda msgs: [
        print(f"received: {struct.unpack('>Q', m)[0]}")
        for m in msgs
    ])
)

status_node = status.inspect(lambda s: print(f"status: {s}"))

wf.Graph([data_node, status_node]).run()
