#!/usr/bin/env python3
"""ZMQ publisher — direct mode.

Publishes a UTF-8 counter string every 500 ms. Cross-language compatible
— the Rust subscriber works too.

Run publisher and subscriber in separate terminals:

    python examples/zmq/direct/zmq_pub.py
    python examples/zmq/direct/zmq_sub.py
"""

import wingfoil as wf

PORT = 7779
print(f"Publishing on tcp://127.0.0.1:{PORT} ...")

(
    wf.ticker(0.5)
    .count()
    .inspect(lambda n: print(f"publishing: {n}"))
    .map(lambda n: str(n).encode())
    .zmq_pub(PORT)
    .run(realtime=True)
)
