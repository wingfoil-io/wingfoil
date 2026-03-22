#!/usr/bin/env python3
"""ZMQ subscriber example: receives and prints data + connection status."""

import struct
import sys
import wingfoil as wf

ADDRESS = "tcp://127.0.0.1:5555"
print(f"Connecting to {ADDRESS} ... (Ctrl-C to exit)")

data, status = wf.py_zmq_sub(ADDRESS)

data_node = data.inspect(lambda msgs: [
    # Each message is raw bytes; decode as little-endian u64 to match
    # the publisher's encoding (struct.pack('<Q') / n.to_le_bytes()).
    print(f"received: {struct.unpack('<Q', m)[0]}", flush=True)
    for m in msgs
])

status_node = status.inspect(lambda s: print(f"status: {s}", flush=True))

try:
    wf.Graph([data_node, status_node]).run()
except KeyboardInterrupt:
    print("\nExiting.")
    sys.exit(0)
