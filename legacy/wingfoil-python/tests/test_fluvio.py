"""Tests for the Fluvio Python bindings.

Integration tests (TestFluvioSub, TestFluvioPub) are skipped automatically
when a Fluvio cluster is not running on localhost:9003. Start a local
cluster with:

    fluvio cluster start --local

The TestFluvioConstruction and TestFluvioUnreachable suites exercise the
Python binding code without requiring a live cluster: stream/node setup,
the dict_to_record marshaling closure, and error propagation when the
SC is unreachable or the start offset is invalid.
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

# An endpoint that's guaranteed not to host a Fluvio SC: TCP reject on loopback.
UNREACHABLE_ENDPOINT = "127.0.0.1:1"


class TestFluvioConstruction(unittest.TestCase):
    """Binding construction paths that don't need a running cluster.

    Exercises py_fluvio_sub / fluvio_pub wiring including default arguments,
    optional start_offset, and the fluent .fluvio_pub() stream method.
    """

    def test_fluvio_sub_default_partition_and_offset(self):
        from wingfoil import fluvio_sub

        stream = fluvio_sub(UNREACHABLE_ENDPOINT, "topic")
        self.assertIsNotNone(stream)

    def test_fluvio_sub_with_partition_and_offset(self):
        from wingfoil import fluvio_sub

        stream = fluvio_sub(
            UNREACHABLE_ENDPOINT, "topic", partition=2, start_offset=100
        )
        self.assertIsNotNone(stream)

    def test_fluvio_pub_method_constructs_node(self):
        from wingfoil import constant

        node = constant({"value": b"hello"}).fluvio_pub(
            UNREACHABLE_ENDPOINT, "topic"
        )
        self.assertIsNotNone(node)

    def test_fluvio_pub_from_list_of_dicts_constructs_node(self):
        from wingfoil import constant

        records = [{"key": "k", "value": b"v"}, {"value": b"v2"}]
        node = constant(records).fluvio_pub(UNREACHABLE_ENDPOINT, "topic")
        self.assertIsNotNone(node)


class TestFluvioUnreachable(unittest.TestCase):
    """Running against an unreachable SC triggers the error paths in the
    async producer / consumer. The map closure for fluvio_pub runs first,
    so dict_to_record is exercised before the connection failure.
    """

    def test_fluvio_sub_negative_offset_errors(self):
        # Validation fails immediately inside produce_async.
        from wingfoil import fluvio_sub

        stream = fluvio_sub(
            UNREACHABLE_ENDPOINT, "topic", partition=0, start_offset=-1
        )
        with self.assertRaises(Exception):
            stream.collect().run(realtime=True, cycles=1)

    def test_fluvio_pub_single_dict_marshals_then_errors(self):
        # dict_to_record runs on the upstream tick; the subsequent connect
        # to the unreachable SC raises — the error surfaces from teardown.
        from wingfoil import constant

        node = constant({"value": b"hello", "key": "k"}).fluvio_pub(
            UNREACHABLE_ENDPOINT, "topic"
        )
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_fluvio_pub_list_of_dicts_marshals_then_errors(self):
        # List-of-dicts path also exercises dict_to_record for each entry.
        from wingfoil import constant

        records = [
            {"key": "k1", "value": b"v1"},
            {"value": b"v2"},  # keyless (covers `key: None` branch)
        ]
        node = constant(records).fluvio_pub(UNREACHABLE_ENDPOINT, "topic")
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_fluvio_pub_bad_value_type_marshals_empty_burst(self):
        # When the upstream value is neither a dict nor a list, the closure
        # logs an error and emits an empty burst. The run still errors at
        # connect time, but the non-dict/list branch is covered.
        from wingfoil import constant

        node = constant("not a dict").fluvio_pub(UNREACHABLE_ENDPOINT, "topic")
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)


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
