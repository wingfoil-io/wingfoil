"""Tests for the OTLP push Python bindings.

Requires a running OpenTelemetry collector on localhost:4318.
All tests are skipped if the collector is not available.

Setup:
    docker run --rm -p 4318:4318 otel/opentelemetry-collector:latest
"""

import socket
import unittest

PORT = 4318


def collector_available():
    try:
        with socket.create_connection(("localhost", PORT), timeout=1):
            return True
    except OSError:
        return False


COLLECTOR_AVAILABLE = collector_available()


@unittest.skipUnless(COLLECTOR_AVAILABLE, "OTel collector not running on localhost:4318")
class TestOtlpPush(unittest.TestCase):
    def test_push_sends_without_error(self):
        """otlp_push runs without raising an exception when collector is available."""
        from wingfoil import ticker

        node = ticker(0.1).count().otlp_push(
            "test_counter",
            endpoint="http://localhost:4318",
            service_name="wingfoil-py-test",
        )
        node.run(realtime=False, cycles=5)

    def test_push_returns_node(self):
        """otlp_push returns a Node object."""
        from wingfoil import ticker
        from wingfoil._wingfoil import Node

        node = ticker(0.1).count().otlp_push(
            "test_node_type",
            endpoint="http://localhost:4318",
            service_name="wingfoil-py-test",
        )
        self.assertIsInstance(node, Node)

    def test_push_stream_directly(self):
        """otlp_push works on a plain stream (not just count())."""
        from wingfoil import constant

        node = constant(42).otlp_push(
            "constant_gauge",
            endpoint="http://localhost:4318",
            service_name="wingfoil-py-test",
        )
        node.run(realtime=False, cycles=1)


if __name__ == "__main__":
    unittest.main()
