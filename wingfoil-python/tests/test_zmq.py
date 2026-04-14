"""Integration tests for ZMQ pub/sub Python bindings.

ZMQ is peer-to-peer — no broker needed. Direct pub/sub tests run without any
external infrastructure. etcd-discovery tests are selected via
`-m requires_etcd` and fail loudly when etcd is not reachable on
localhost:2379 (they do not silently skip).

Setup (etcd-discovery tests only):
    docker run --rm -p 2379:2379 \\
      -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \\
      -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \\
      gcr.io/etcd-development/etcd:v3.5.0
"""

import threading
import time
import unittest

import pytest

import wingfoil as wf

DIRECT_PORT = 5570


class TestZmqSub(unittest.TestCase):
    def test_sub_returns_bytes_list(self):
        """zmq_sub data stream yields list[bytes] on each tick."""
        port = DIRECT_PORT

        def _run_publisher():
            (
                wf.ticker(0.05)
                .count()
                .map(lambda n: str(n).encode())
                .zmq_pub(port)
                .run(realtime=True, duration=1.5)
            )

        pub_thread = threading.Thread(target=_run_publisher, daemon=True)
        pub_thread.start()
        time.sleep(0.3)  # let publisher bind

        data, _status = wf.zmq_sub(f"tcp://127.0.0.1:{port}")
        items = []
        data.inspect(lambda msgs: items.extend(msgs)).run(
            realtime=True, duration=0.8
        )

        self.assertGreater(len(items), 0, "no messages received")
        for item in items:
            self.assertIsInstance(item, bytes)

        pub_thread.join(timeout=2.0)

    def test_sub_receives_consecutive_integers(self):
        """Subscriber receives a consecutive counter sequence from the publisher."""
        port = DIRECT_PORT + 1

        def _run_publisher():
            (
                wf.ticker(0.05)
                .count()
                .map(lambda n: str(n).encode())
                .zmq_pub(port)
                .run(realtime=True, duration=1.5)
            )

        pub_thread = threading.Thread(target=_run_publisher, daemon=True)
        pub_thread.start()
        time.sleep(0.3)

        data, _status = wf.zmq_sub(f"tcp://127.0.0.1:{port}")
        items = []
        data.inspect(lambda msgs: items.extend(msgs)).run(
            realtime=True, duration=0.8
        )

        nums = [int(b) for b in items]
        self.assertGreaterEqual(len(nums), 3, f"expected >= 3 items, got {len(nums)}")
        for a, b in zip(nums, nums[1:]):
            self.assertEqual(b, a + 1, f"non-consecutive: {a}, {b}")

        pub_thread.join(timeout=2.0)

    def test_status_stream_yields_strings(self):
        """zmq_sub status stream yields 'connected' or 'disconnected'."""
        port = DIRECT_PORT + 2

        def _run_publisher():
            (
                wf.ticker(0.05)
                .count()
                .map(lambda n: str(n).encode())
                .zmq_pub(port)
                .run(realtime=True, duration=1.5)
            )

        pub_thread = threading.Thread(target=_run_publisher, daemon=True)
        pub_thread.start()
        time.sleep(0.3)

        _data, status = wf.zmq_sub(f"tcp://127.0.0.1:{port}")
        statuses = []
        status.inspect(lambda s: statuses.append(s)).run(
            realtime=True, duration=0.8
        )

        self.assertGreater(len(statuses), 0, "no status events received")
        for s in statuses:
            self.assertIn(s, ("connected", "disconnected"))

        pub_thread.join(timeout=2.0)


class TestZmqPub(unittest.TestCase):
    def test_pub_round_trip(self):
        """Publisher sends data that subscriber receives correctly."""
        port = DIRECT_PORT + 3

        def _run_publisher():
            (
                wf.ticker(0.05)
                .count()
                .map(lambda n: str(n).encode())
                .zmq_pub(port)
                .run(realtime=True, duration=1.5)
            )

        pub_thread = threading.Thread(target=_run_publisher, daemon=True)
        pub_thread.start()
        time.sleep(0.3)

        data, _status = wf.zmq_sub(f"tcp://127.0.0.1:{port}")
        items = []
        data.inspect(lambda msgs: items.extend(msgs)).run(
            realtime=True, duration=0.8
        )

        self.assertGreater(len(items), 0, "no data received in round trip")
        # Verify all items are valid integer strings
        for item in items:
            int(item)  # raises ValueError if not a valid integer

        pub_thread.join(timeout=2.0)

    def test_deserialization_error_propagates(self):
        """Raw non-bincode bytes from a plain ZMQ socket cause an error."""
        port = DIRECT_PORT + 4

        def _publish_garbage():
            import zmq as pyzmq

            ctx = pyzmq.Context()
            sock = ctx.socket(pyzmq.PUB)
            sock.bind(f"tcp://127.0.0.1:{port}")
            time.sleep(0.3)
            for _ in range(20):
                sock.send(b"not valid bincode")
                time.sleep(0.05)
            sock.close()
            ctx.term()

        t = threading.Thread(target=_publish_garbage, daemon=True)
        t.start()
        time.sleep(0.3)

        data, _status = wf.zmq_sub(f"tcp://127.0.0.1:{port}")
        with self.assertRaises(Exception):
            data.inspect(lambda _: None).run(realtime=True, duration=3.0)


# --- etcd discovery tests ---


ETCD_ENDPOINT = "http://127.0.0.1:2379"
ETCD_PUB_PORT = 5592
ETCD_SERVICE = "pytest/etcd-quotes"


@pytest.mark.requires_etcd
class TestZmqEtcdDiscovery(unittest.TestCase):
    def test_zmq_sub_etcd_no_etcd_returns_error(self):
        """Connection refused when etcd is unreachable."""
        with self.assertRaises(Exception):
            wf.zmq_sub_etcd("anything", "http://127.0.0.1:59999")

    def test_zmq_pub_etcd_end_to_end(self):
        """Full round-trip using etcd discovery."""

        def _run_publisher():
            (
                wf.ticker(0.05)
                .count()
                .map(lambda v: str(v).encode())
                .zmq_pub_etcd(ETCD_SERVICE, ETCD_PUB_PORT, ETCD_ENDPOINT)
                .run(realtime=True, duration=0.7)
            )

        pub_thread = threading.Thread(target=_run_publisher, daemon=True)
        pub_thread.start()
        time.sleep(0.3)

        data, _status = wf.zmq_sub_etcd(ETCD_SERVICE, ETCD_ENDPOINT)
        items = []
        data.inspect(lambda v: items.extend(v)).run(realtime=True, duration=0.5)

        self.assertGreater(len(items), 0, "no data received via etcd discovery")
        pub_thread.join(timeout=2.0)


if __name__ == "__main__":
    unittest.main()
