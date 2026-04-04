import threading
import time

import pytest
import zmq

import wingfoil as wf

PORT = 5570


def _publish_garbage(port, ready):
    """Publish raw bytes that cannot be deserialized as Message<Vec<u8>>."""
    ctx = zmq.Context()
    sock = ctx.socket(zmq.PUB)
    sock.bind(f"tcp://127.0.0.1:{port}")
    ready.set()
    time.sleep(0.3)  # let subscriber connect (slow-joiner)
    for _ in range(20):
        sock.send(b"not valid bincode")
        time.sleep(0.05)
    sock.close()
    ctx.term()


def test_deserialization_error_propagates():
    ready = threading.Event()
    t = threading.Thread(target=_publish_garbage, args=(PORT, ready), daemon=True)
    t.start()
    ready.wait()

    data, _status = wf.py_zmq_sub(f"tcp://127.0.0.1:{PORT}")
    data_node = data.inspect(lambda _: None)

    with pytest.raises(Exception):
        wf.Graph([data_node]).run(duration=3.0)


# --- etcd discovery tests ---
# Skipped unless etcd is reachable on localhost:2379.

ETCD_ENDPOINT = "http://127.0.0.1:2379"
ETCD_PUB_PORT = 5592
ETCD_SERVICE = "pytest/etcd-quotes"


def _etcd_available():
    """Return True if etcd is reachable (for test skipping)."""
    try:
        import socket

        s = socket.create_connection(("127.0.0.1", 2379), timeout=0.5)
        s.close()
        return True
    except OSError:
        return False


@pytest.mark.skipif(
    not _etcd_available() or wf.zmq_sub_etcd is None,
    reason="etcd not available on localhost:2379 or etcd feature not compiled",
)
class TestZmqEtcdDiscovery:
    def test_zmq_sub_etcd_no_etcd_returns_error(self):
        with pytest.raises(Exception):
            wf.zmq_sub_etcd("anything", "http://127.0.0.1:59999")

    def test_zmq_pub_etcd_end_to_end(self):
        """Full round-trip using etcd discovery."""

        def _run_publisher():
            node = (
                wf.ticker(0.05)
                .count()
                .map(lambda v: str(v).encode())
                .zmq_pub_etcd(ETCD_SERVICE, ETCD_PUB_PORT, ETCD_ENDPOINT)
            )
            node.run(realtime=True, duration=0.7)

        pub_thread = threading.Thread(target=_run_publisher, daemon=True)
        pub_thread.start()
        time.sleep(0.3)  # wait for publisher to register in etcd

        data, _status = wf.zmq_sub_etcd(ETCD_SERVICE, ETCD_ENDPOINT)
        items = []
        data.inspect(lambda v: items.extend(v)).run(realtime=True, duration=0.5)

        assert len(items) > 0, "no data received via etcd discovery"
        pub_thread.join(timeout=2.0)
