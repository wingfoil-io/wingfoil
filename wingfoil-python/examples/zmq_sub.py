#!/usr/bin/env python3
"""ZMQ subscriber example: receives and prints data + connection status."""

import struct
import sys
import threading
import wingfoil as wf

ADDRESS = "tcp://127.0.0.1:5555"
print(f"Connecting to {ADDRESS} ... (Ctrl-C to exit)")

data, status = wf.py_zmq_sub(ADDRESS)

data_node = data.inspect(lambda msgs: [
    print(f"received: {struct.unpack('>Q', m)[0]}")
    for m in msgs
])

status_node = status.inspect(lambda s: print(f"status: {s}"))

# Run the graph in a daemon thread so Ctrl-C on the main thread terminates cleanly.
graph = wf.Graph([data_node, status_node])
t = threading.Thread(target=graph.run, daemon=True)
t.start()
try:
    t.join()
except KeyboardInterrupt:
    print("\nExiting.")
    sys.exit(0)
