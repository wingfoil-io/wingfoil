#!/usr/bin/env python3
"""ZMQ subscriber — etcd discovery mode.

Looks the publisher's address up from etcd under SERVICE_NAME. The lookup
happens at **wiring** time, so an unreachable registry raises here rather than
failing the run — start `zmq_pub.py` first.

Prerequisites — start etcd locally:

    docker run --rm -p 2379:2379 \
      -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
      -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
      gcr.io/etcd-development/etcd:v3.5.0

Run publisher and subscriber in separate terminals:

    cd crates/wingfoil-python && maturin develop --features zmq,etcd
    python examples/zmq/etcd/zmq_pub.py
    python examples/zmq/etcd/zmq_sub.py
"""

import sys

import wingfoil as wf

ETCD_ENDPOINT = "http://127.0.0.1:2379"
SERVICE_NAME = "zmq-python-example/quotes"
print(f"Looking up '{SERVICE_NAME}' in etcd ... (Ctrl-C to exit)")

g = wf.Graph()

data, status = wf.zmq_sub_etcd(g, SERVICE_NAME, ETCD_ENDPOINT)

data.inspect(
    lambda msgs: [print(f"received: {m.decode()}", flush=True) for m in msgs]
)
status.inspect(lambda s: print(f"status: {s}", flush=True))

try:
    g.run(realtime=True)
except KeyboardInterrupt:
    print("\nExiting.")
    sys.exit(0)
