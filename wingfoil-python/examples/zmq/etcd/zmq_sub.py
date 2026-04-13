#!/usr/bin/env python3
"""ZMQ subscriber — etcd discovery mode.

Looks up the publisher address from etcd under SERVICE_NAME.
Cross-language compatible — the Rust publisher works too.

Prerequisites — start etcd locally:

    docker run --rm -p 2379:2379 \
      -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
      -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
      gcr.io/etcd-development/etcd:v3.5.0

Run publisher and subscriber in separate terminals:

    python examples/zmq/etcd/zmq_pub.py
    python examples/zmq/etcd/zmq_sub.py
"""

import sys
import wingfoil as wf

ETCD_ENDPOINT = "http://127.0.0.1:2379"
SERVICE_NAME = "zmq-python-example/quotes"
print(f"Looking up '{SERVICE_NAME}' in etcd ... (Ctrl-C to exit)")

data, status = wf.zmq_sub_etcd(SERVICE_NAME, ETCD_ENDPOINT)

data_node = data.inspect(lambda msgs: [
    print(f"received: {m.decode()}", flush=True) for m in msgs
])

status_node = status.inspect(lambda s: print(f"status: {s}", flush=True))

try:
    wf.Graph([data_node, status_node]).run(realtime=True)
except KeyboardInterrupt:
    print("\nExiting.")
    sys.exit(0)
