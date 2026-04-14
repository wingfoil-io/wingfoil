"""Tests for the OTLP push Python bindings.

The always-on class runs in every Python test job. The `TestOtlpPush` class
is selected via `-m requires_otel` and fails loudly when a collector is not
reachable on localhost:4318.

Setup (only needed for `-m requires_otel`):
    docker run --rm -p 4318:4318 otel/opentelemetry-collector:latest
"""

import unittest

import pytest

PORT = 4318


class TestOtlpPushAlways(unittest.TestCase):
    """Tests that run without an external collector."""

    def test_historical_mode_drains_without_connecting(self):
        """In historical mode otlp_push returns without making any network calls."""
        from wingfoil import ticker

        # Points at a port nothing is listening on — historical mode must not connect.
        node = ticker(0.01).count().otlp_push(
            "hist_metric",
            endpoint="http://127.0.0.1:1",
            service_name="wingfoil-py-test",
        )
        # historical mode: start=0.0 → HistoricalFrom(NanoTime::ZERO)
        node.run(realtime=False, start=0.0, cycles=5)

    def test_otlp_push_returns_node(self):
        """otlp_push returns a Node object."""
        from wingfoil import ticker
        from wingfoil._wingfoil import Node

        node = ticker(0.1).count().otlp_push(
            "type_check_metric",
            endpoint="http://127.0.0.1:1",
            service_name="wingfoil-py-test",
        )
        self.assertIsInstance(node, Node)


@pytest.mark.requires_otel
class TestOtlpPush(unittest.TestCase):
    def test_push_sends_without_error(self):
        """otlp_push runs without raising an exception when collector is available."""
        from wingfoil import ticker

        node = ticker(0.1).count().otlp_push(
            "test_counter",
            endpoint="http://localhost:4318",
            service_name="wingfoil-py-test",
        )
        node.run(realtime=True, cycles=5)

    def test_push_stream_directly(self):
        """otlp_push works on a plain stream (not just count())."""
        from wingfoil import constant

        node = constant(42).otlp_push(
            "constant_gauge",
            endpoint="http://localhost:4318",
            service_name="wingfoil-py-test",
        )
        node.run(realtime=True, cycles=1)


if __name__ == "__main__":
    unittest.main()
