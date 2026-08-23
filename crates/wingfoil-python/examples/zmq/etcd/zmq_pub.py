#!/usr/bin/env python3
"""ZMQ publisher — etcd discovery mode.

Publishes a UTF-8 counter string every 500 ms and registers its address under
SERVICE_NAME in etcd, so subscribers find it by name instead of by port.
Cross-language compatible — the Rust subscriber resolves the same key.

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

import wingfoil as wf

ETCD_ENDPOINT = "http://127.0.0.1:2379"
SERVICE_NAME = "zmq-python-example/quotes"
PORT = 7779
print(f"Publishing on port {PORT}, registered as '{SERVICE_NAME}' in etcd ...")

g = wf.Graph()

wf.zmq_pub_etcd(
    g.counter(period_nanos=500_000_000)
    .inspect(lambda n: print(f"publishing: {n}", flush=True))
    .map(lambda n: str(n).encode()),
    PORT,
    SERVICE_NAME,
    ETCD_ENDPOINT,
)

g.run(realtime=True)
