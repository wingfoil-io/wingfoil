"""Integration tests for FIX protocol Python bindings.

Tests use a self-contained loopback (acceptor + initiator in the same process),
so no external FIX engine is required.
"""

import threading
import time
import unittest


class TestFixLoopback(unittest.TestCase):
    def test_connect_and_accept_loopback(self):
        """fix_accept + fix_connect establish a session and exchange status events."""
        from wingfoil import fix_accept, fix_connect

        port = 19877

        # Start acceptor in a background thread
        results = {"acc_status": None, "init_status": None}

        def run_acceptor():
            acc_data, acc_status = fix_accept(port, "ACCEPTOR", "INITIATOR")
            status_stream = acc_status.collect()
            from wingfoil import Graph

            Graph([acc_data, status_stream]).run(realtime=True, duration=2.0)
            results["acc_status"] = status_stream.peek_value()

        acc_thread = threading.Thread(target=run_acceptor, daemon=True)
        acc_thread.start()

        # Give acceptor time to bind
        time.sleep(0.3)

        # Run initiator
        init_data, init_status = fix_connect("127.0.0.1", port, "INITIATOR", "ACCEPTOR")
        status_stream = init_status.collect()
        from wingfoil import Graph

        Graph([init_data, status_stream]).run(realtime=True, duration=1.5)
        results["init_status"] = status_stream.peek_value()

        acc_thread.join(timeout=5)

        # Initiator should have received at least one status event
        init_statuses = results["init_status"]
        self.assertIsNotNone(init_statuses)
        all_statuses = [s for tick in init_statuses for s in tick]
        self.assertTrue(
            any(s == "logged_in" for s in all_statuses),
            f"Expected 'logged_in' in initiator statuses, got: {all_statuses}",
        )

    def test_fix_connect_returns_tuple(self):
        """fix_connect returns a (data_stream, status_stream) tuple."""
        from wingfoil import fix_connect

        result = fix_connect("127.0.0.1", 1, "S", "T")
        self.assertIsInstance(result, tuple)
        self.assertEqual(len(result), 2)

    def test_fix_accept_returns_tuple(self):
        """fix_accept returns a (data_stream, status_stream) tuple."""
        from wingfoil import fix_accept

        result = fix_accept(19878, "S", "T")
        self.assertIsInstance(result, tuple)
        self.assertEqual(len(result), 2)

    def test_fix_connect_tls_returns_triple(self):
        """fix_connect_tls returns a (data_stream, status_stream, sender) triple."""
        from wingfoil import fix_connect_tls

        result = fix_connect_tls("127.0.0.1", 443, "S", "T")
        self.assertIsInstance(result, tuple)
        self.assertEqual(len(result), 3)


if __name__ == "__main__":
    unittest.main()
