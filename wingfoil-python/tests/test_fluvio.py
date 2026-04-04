"""Integration tests for the Fluvio Python bindings.

Tests are skipped automatically when a Fluvio cluster is not running on
localhost:9003. Start a local cluster with:

    fluvio cluster start --local

Or run the SC in Docker and ensure the SPU is also registered.
"""

import socket
import urllib.request
import unittest

FLUVIO_ENDPOINT = "127.0.0.1:9003"
FLUVIO_HOST = "127.0.0.1"
FLUVIO_SC_PORT = 9003


def fluvio_available() -> bool:
    """Return True if a Fluvio SC is accepting connections on localhost:9003."""
    try:
        with socket.create_connection((FLUVIO_HOST, FLUVIO_SC_PORT), timeout=1):
            return True
    except OSError:
        return False


FLUVIO_AVAILABLE = fluvio_available()


@unittest.skipUnless(FLUVIO_AVAILABLE, f"Fluvio not running on {FLUVIO_ENDPOINT}")
class TestFluvioSub(unittest.TestCase):
    """Tests for py_fluvio_sub / fluvio_sub."""

    def test_sub_returns_list(self):
        """fluvio_sub emits a list of event dicts per tick."""
        from wingfoil import fluvio_sub

        # Topic must already exist in the running cluster.
        # This test just verifies that the stream can be constructed and run.
        stream = fluvio_sub(FLUVIO_ENDPOINT, "test-python-sub", partition=0).collect()
        # RunFor::Cycles(0) — just verify setup doesn't raise
        stream.run(realtime=True, cycles=0)

    def test_sub_event_dict_shape(self):
        """Each event dict has 'key', 'value', and 'offset' fields."""
        from wingfoil import fluvio_sub

        stream = (
            fluvio_sub(FLUVIO_ENDPOINT, "test-python-sub", partition=0)
            .collect()
        )
        stream.run(realtime=True, cycles=1)
        result = stream.peek_value()
        self.assertIsInstance(result, list)
        if result:  # only validate shape if records exist
            event = result[0]
            self.assertIsInstance(event, dict)
            self.assertIn("key", event)
            self.assertIn("value", event)
            self.assertIn("offset", event)
            self.assertIsInstance(event["value"], bytes)
            self.assertIsInstance(event["offset"], int)


@unittest.skipUnless(FLUVIO_AVAILABLE, f"Fluvio not running on {FLUVIO_ENDPOINT}")
class TestFluvioPub(unittest.TestCase):
    """Tests for the .fluvio_pub() stream method."""

    def test_pub_single_dict(self):
        """A single dict with 'value' bytes is published without error."""
        from wingfoil import constant

        constant({"value": b"hello from python"}).fluvio_pub(
            FLUVIO_ENDPOINT, "test-python-pub"
        ).run(realtime=True, cycles=1)

    def test_pub_keyed_dict(self):
        """A dict with both 'key' and 'value' is published without error."""
        from wingfoil import constant

        constant({"key": "my-key", "value": b"keyed value"}).fluvio_pub(
            FLUVIO_ENDPOINT, "test-python-pub"
        ).run(realtime=True, cycles=1)

    def test_pub_list_of_dicts(self):
        """A list of dicts is published as multiple records without error."""
        from wingfoil import constant

        records = [
            {"key": "k1", "value": b"first"},
            {"key": "k2", "value": b"second"},
        ]
        constant(records).fluvio_pub(
            FLUVIO_ENDPOINT, "test-python-pub"
        ).run(realtime=True, cycles=1)


if __name__ == "__main__":
    unittest.main()
