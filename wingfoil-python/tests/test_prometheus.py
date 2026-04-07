"""Tests for the Prometheus exporter Python bindings."""

import unittest
import urllib.request

from wingfoil import PrometheusExporter, ticker


class TestPrometheusExporter(unittest.TestCase):
    def test_serve_and_register(self):
        """PrometheusExporter serves /metrics with the registered metric name."""
        exporter = PrometheusExporter("127.0.0.1:0")  # port 0 = OS assigns
        port = exporter.serve()
        stream = ticker(0.1).count()
        node = exporter.register("test_counter", stream)
        node.run(realtime=True, cycles=5)

        resp = urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics")
        body = resp.read().decode()
        self.assertIn("test_counter", body)

    def test_serve_returns_port(self):
        """serve() returns a non-zero port number."""
        exporter = PrometheusExporter("127.0.0.1:0")
        port = exporter.serve()
        self.assertIsInstance(port, int)
        self.assertGreater(port, 0)

    def test_register_returns_node(self):
        """register() returns a Node that can be run."""
        from wingfoil._wingfoil import Node

        exporter = PrometheusExporter("127.0.0.1:0")
        exporter.serve()
        stream = ticker(0.1).count()
        node = exporter.register("my_gauge", stream)
        self.assertIsInstance(node, Node)

    def test_metrics_endpoint_contains_type_line(self):
        """The /metrics body contains a # TYPE line for the registered metric."""
        exporter = PrometheusExporter("127.0.0.1:0")
        port = exporter.serve()
        stream = ticker(0.1).count()
        node = exporter.register("gauge_metric", stream)
        node.run(realtime=True, cycles=3)

        resp = urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics")
        body = resp.read().decode()
        self.assertIn("# TYPE gauge_metric gauge", body)

    def test_unknown_path_returns_404(self):
        """Requests to paths other than /metrics return 404."""
        exporter = PrometheusExporter("127.0.0.1:0")
        port = exporter.serve()

        import socket
        import time

        # Give the server thread a moment to start
        time.sleep(0.05)

        conn = socket.create_connection(("127.0.0.1", port))
        conn.sendall(b"GET /other HTTP/1.0\r\n\r\n")
        response = b""
        while True:
            chunk = conn.recv(4096)
            if not chunk:
                break
            response += chunk
        conn.close()
        self.assertTrue(response.startswith(b"HTTP/1.1 404"), response[:40])

    def test_historical_mode_produces_no_metrics(self):
        """In historical (backtesting) mode no metrics are written to the store."""
        exporter = PrometheusExporter("127.0.0.1:0")
        port = exporter.serve()
        stream = ticker(0.01).count()
        node = exporter.register("hist_counter", stream)
        # historical mode: start=0.0 means HistoricalFrom(NanoTime::ZERO)
        node.run(realtime=False, start=0.0, cycles=5)

        resp = urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics")
        body = resp.read().decode()
        self.assertEqual(body, "", f"expected empty body in historical mode, got: {body!r}")

    def test_metrics_are_sorted_alphabetically(self):
        """Multiple registered metrics appear in alphabetical order in the output."""
        exporter = PrometheusExporter("127.0.0.1:0")
        port = exporter.serve()
        stream = ticker(0.1).count()
        node_b = exporter.register("metric_b", stream)
        node_a = exporter.register("metric_a", stream)

        from wingfoil import Graph
        Graph([node_a, node_b]).run(realtime=True, cycles=3)

        resp = urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics")
        body = resp.read().decode()
        pos_a = body.find("metric_a")
        pos_b = body.find("metric_b")
        self.assertGreater(pos_a, -1, "metric_a not found")
        self.assertGreater(pos_b, -1, "metric_b not found")
        self.assertLess(pos_a, pos_b, "metric_a should appear before metric_b")


if __name__ == "__main__":
    unittest.main()
