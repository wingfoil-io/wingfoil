#!/usr/bin/env python3
"""ZMQ subscriber — direct mode.

Connects to the publisher at ADDRESS; start `zmq_pub.py` first. Cross-language
compatible — a Rust publisher works too.

`zmq_sub` returns a **pair** of streams: the data, and a connection-status
stream that ticks on connect/disconnect. Each data tick is a burst — a list of
`bytes` that arrived together — never a latest-wins single value.

Run publisher and subscriber in separate terminals:

    cd crates/wingfoil-python && maturin develop --features zmq
    python examples/zmq/direct/zmq_pub.py
    python examples/zmq/direct/zmq_sub.py
"""

import sys

import wingfoil as wf

ADDRESS = "tcp://127.0.0.1:7779"
print(f"Connecting to {ADDRESS} ... (Ctrl-C to exit)")

g = wf.Graph()

data, status = wf.zmq_sub(g, ADDRESS)

data.inspect(
    lambda msgs: [print(f"received: {m.decode()}", flush=True) for m in msgs]
)
status.inspect(lambda s: print(f"status: {s}", flush=True))

try:
    g.run(realtime=True)
except KeyboardInterrupt:
    print("\nExiting.")
    sys.exit(0)
