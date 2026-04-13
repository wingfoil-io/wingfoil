#!/usr/bin/env python3
"""ZMQ subscriber — direct mode.

Connects to the publisher at ADDRESS. Start zmq_pub.py first.
Cross-language compatible — the Rust publisher works too.

Run publisher and subscriber in separate terminals:

    python examples/zmq/direct/zmq_pub.py
    python examples/zmq/direct/zmq_sub.py
"""

import sys
import wingfoil as wf

ADDRESS = "tcp://127.0.0.1:7779"
print(f"Connecting to {ADDRESS} ... (Ctrl-C to exit)")

data, status = wf.zmq_sub(ADDRESS)

data_node = data.inspect(lambda msgs: [
    print(f"received: {m.decode()}", flush=True) for m in msgs
])

status_node = status.inspect(lambda s: print(f"status: {s}", flush=True))

try:
    wf.Graph([data_node, status_node]).run(realtime=True)
except KeyboardInterrupt:
    print("\nExiting.")
    sys.exit(0)
