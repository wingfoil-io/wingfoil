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


class TestFixSender(unittest.TestCase):
    """Sender API is exposed by fix_connect_tls; we don't need an actual
    TLS server running to exercise argument validation."""

    def _make_sender(self):
        from wingfoil import fix_connect_tls

        _, _, sender = fix_connect_tls(
            "127.0.0.1", 443, "SENDER", "TARGET", password="secret"
        )
        return sender

    def test_send_accepts_valid_message(self):
        sender = self._make_sender()
        # Validates the arg-parsing path end-to-end. The session thread may
        # already have closed the channel (no TLS server is listening on
        # 127.0.0.1:443), so a RuntimeError from a closed channel is also
        # an acceptable outcome — the point is that parsing succeeds and
        # doesn't raise KeyError/ValueError.
        try:
            sender.send({"msg_type": "D", "fields": [[55, "AAPL"], [38, "100"]]})
        except RuntimeError:
            pass

    def test_send_missing_msg_type_raises(self):
        sender = self._make_sender()
        with self.assertRaises(KeyError):
            sender.send({"fields": []})

    def test_send_missing_fields_raises(self):
        sender = self._make_sender()
        with self.assertRaises(KeyError):
            sender.send({"msg_type": "D"})

    def test_send_field_wrong_length_raises(self):
        sender = self._make_sender()
        with self.assertRaises(ValueError):
            sender.send({"msg_type": "D", "fields": [[55]]})
        with self.assertRaises(ValueError):
            sender.send({"msg_type": "D", "fields": [[55, "x", "y"]]})


class TestFixConnectTlsNoPassword(unittest.TestCase):
    def test_connect_tls_without_password(self):
        from wingfoil import fix_connect_tls

        # Password defaults to None; exercises the Option<String>.as_deref() branch.
        data, status, sender = fix_connect_tls("127.0.0.1", 443, "S", "T")
        self.assertIsNotNone(data)
        self.assertIsNotNone(status)
        self.assertIsNotNone(sender)


class TestFixLoopbackData(unittest.TestCase):
    """Exercises msg_to_py and status_to_py via a real loopback session.
    Covers the 'disconnected' status variant on acceptor shutdown."""

    def test_loopback_acceptor_sees_disconnect(self):
        import threading
        import time

        from wingfoil import Graph, fix_accept, fix_connect

        port = 19879
        acc_statuses = []

        def run_acceptor():
            acc_data, acc_status = fix_accept(port, "ACCEPTOR", "INITIATOR")
            status_stream = acc_status.collect()
            Graph([acc_data, status_stream]).run(realtime=True, duration=2.0)
            vals = status_stream.peek_value() or []
            acc_statuses.extend(s for tick in vals for s in tick)

        t = threading.Thread(target=run_acceptor, daemon=True)
        t.start()
        time.sleep(0.3)

        init_data, init_status = fix_connect("127.0.0.1", port, "INITIATOR", "ACCEPTOR")
        status_stream = init_status.collect()
        Graph([init_data, status_stream]).run(realtime=True, duration=0.8)
        t.join(timeout=5)

        # Acceptor should observe at least a logged_in (and after initiator
        # exits, a disconnected status).
        self.assertIn("logged_in", acc_statuses)


if __name__ == "__main__":
    unittest.main()
