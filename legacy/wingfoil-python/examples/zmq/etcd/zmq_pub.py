#!/usr/bin/env python3
"""ZMQ publisher — etcd discovery mode.

Publishes a UTF-8 counter string every 500 ms and registers under SERVICE_NAME
in etcd. Cross-language compatible — the Rust subscriber works too.

Prerequisites — start etcd locally:

    docker run --rm -p 2379:2379 \
      -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
      -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
      gcr.io/etcd-development/etcd:v3.5.0

Run publisher and subscriber in separate terminals:

    python examples/zmq/etcd/zmq_pub.py
    python examples/zmq/etcd/zmq_sub.py
"""

import wingfoil as wf

ETCD_ENDPOINT = "http://127.0.0.1:2379"
SERVICE_NAME = "zmq-python-example/quotes"
PORT = 7779
print(f"Publishing on port {PORT}, registered as '{SERVICE_NAME}' in etcd ...")

(
    wf.ticker(0.5)
    .count()
    .inspect(lambda n: print(f"publishing: {n}"))
    .map(lambda n: str(n).encode())
    .zmq_pub_etcd(SERVICE_NAME, PORT, ETCD_ENDPOINT)
    .run(realtime=True)
)
