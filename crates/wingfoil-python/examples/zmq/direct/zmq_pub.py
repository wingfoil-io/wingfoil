#!/usr/bin/env python3
"""ZMQ publisher — direct mode.

Publishes a UTF-8 counter string every 500 ms. Cross-language compatible — the
Rust subscriber reads this, and `zmq_sub.py` reads a Rust publisher.

Run publisher and subscriber in separate terminals:

    cd crates/wingfoil-python && maturin develop --features zmq
    python examples/zmq/direct/zmq_pub.py
    python examples/zmq/direct/zmq_sub.py
"""

import wingfoil as wf

PORT = 7779
print(f"Publishing on tcp://127.0.0.1:{PORT} ...")

g = wf.Graph()

wf.zmq_pub(
    g.counter(period_nanos=500_000_000)
    .inspect(lambda n: print(f"publishing: {n}", flush=True))
    .map(lambda n: str(n).encode()),
    PORT,
)

g.run(realtime=True)
