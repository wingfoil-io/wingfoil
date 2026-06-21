"""Tests for the Redis Pub/Sub Python bindings.

Two groups:

- Unit-level construction / marshaling tests (no marker): run by default, with no
  live service. They keep the binding module visible in coverage and exercise the
  pyo3 marshaling glue.
- Integration tests (``@pytest.mark.requires_redis``): deselected by default. The
  Redis integration workflow opts in with ``-m requires_redis``; without Redis on
  localhost:6379 they fail loudly with a real connection error rather than skipping.

Local setup:
    docker run --rm -p 6379:6379 redis:7-alpine
"""

import threading
import time
import unittest

import pytest

ENDPOINT = "redis://localhost:6379"
# Loopback port 1 is reserved and never hosts a service: connect is rejected.
UNREACHABLE_ENDPOINT = "redis://127.0.0.1:1"
CHANNEL_PREFIX = "wingfoil_pytest_"


# ---- Unit-level coverage (no live Redis) ----


class TestRedisConstruction(unittest.TestCase):
    def test_sub_constructs_stream(self):
        from wingfoil import redis_sub

        stream = redis_sub(UNREACHABLE_ENDPOINT, "ch")
        self.assertIsNotNone(stream)

    def test_pub_method_constructs_node(self):
        from wingfoil import constant

        node = constant({"channel": "ch", "payload": b"v"}).redis_pub(
            UNREACHABLE_ENDPOINT, "ch"
        )
        self.assertIsNotNone(node)


class TestRedisUnreachable(unittest.TestCase):
    def test_pub_single_dict_marshals_then_errors(self):
        # dict_to_entry runs on the upstream tick; connect then fails.
        from wingfoil import constant

        node = constant({"channel": "ch", "payload": b"v"}).redis_pub(
            UNREACHABLE_ENDPOINT, "ch"
        )
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_pub_list_of_dicts_marshals_then_errors(self):
        from wingfoil import constant

        records = [{"channel": "ch", "payload": b"v"}, {"payload": b"v2"}]  # channel-less path
        node = constant(records).redis_pub(UNREACHABLE_ENDPOINT, "ch")
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_pub_bad_value_type_marshals_empty_burst(self):
        # Fallthrough branch: neither dict nor list. Logs an error and emits an
        # empty burst; the run still fails at connect time.
        from wingfoil import constant

        node = constant("not a dict").redis_pub(UNREACHABLE_ENDPOINT, "ch")
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_sub_unreachable_errors(self):
        from wingfoil import redis_sub

        stream = redis_sub(UNREACHABLE_ENDPOINT, "ch").collect()
        with self.assertRaises(Exception):
            stream.run(realtime=True, cycles=1)

    def test_stream_read_constructs_stream(self):
        from wingfoil import redis_stream_read

        stream = redis_stream_read(UNREACHABLE_ENDPOINT, "events")
        self.assertIsNotNone(stream)

    def test_stream_write_marshals_then_errors(self):
        # dict_to_record runs on the upstream tick; connect then fails.
        from wingfoil import constant

        node = constant({"key": "events", "fields": {"kind": b"login"}}).redis_stream_write(
            UNREACHABLE_ENDPOINT, "events"
        )
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_stream_write_list_marshals_then_errors(self):
        from wingfoil import constant

        records = [
            {"key": "events", "fields": {"kind": b"login"}},
            {"fields": {"kind": b"logout"}},  # key-less path
        ]
        node = constant(records).redis_stream_write(UNREACHABLE_ENDPOINT, "events")
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)


# ---- Integration tests (require live Redis) ----


@pytest.mark.requires_redis
class TestRedisSub(unittest.TestCase):
    def test_sub_returns_expected_shape(self):
        from wingfoil import redis_sub

        channel = f"{CHANNEL_PREFIX}sub_shape"
        stream = redis_sub(ENDPOINT, channel).collect()
        stream.run(realtime=True, duration=2.0)
        result = stream.peek_value()
        # Nobody publishes here, so the upstream may never tick: None or list.
        self.assertTrue(result is None or isinstance(result, list))


@pytest.mark.requires_redis
class TestRedisPub(unittest.TestCase):
    def test_pub_to_no_subscriber_succeeds(self):
        # PUBLISH with no subscribers returns 0 receivers, not an error; this
        # proves the pub path connects to a live Redis.
        from wingfoil import constant

        channel = f"{CHANNEL_PREFIX}pub_noop"
        constant({"channel": channel, "payload": b"v"}).redis_pub(
            ENDPOINT, channel
        ).run(realtime=True, cycles=1)

    def test_pub_sub_round_trip(self):
        from wingfoil import constant, redis_sub

        channel = f"{CHANNEL_PREFIX}round_trip"
        received = []

        def subscriber():
            stream = redis_sub(ENDPOINT, channel).collect()
            stream.run(realtime=True, duration=3.0)
            for tick in stream.peek_value() or []:
                for event in tick:
                    received.append(event["payload"])

        t = threading.Thread(target=subscriber)
        t.start()
        # Let the subscriber register before publishing (fire-and-forget).
        time.sleep(0.8)
        constant({"channel": channel, "payload": b"hello_from_python"}).redis_pub(
            ENDPOINT, channel
        ).run(realtime=True, cycles=1)
        t.join()

        self.assertIn(b"hello_from_python", received)


@pytest.mark.requires_redis
class TestRedisStreams(unittest.TestCase):
    def test_stream_write_and_read_snapshot(self):
        from wingfoil import constant, redis_stream_read

        key = f"{CHANNEL_PREFIX}stream_snapshot"
        constant(
            [
                {"key": key, "fields": {"kind": b"login"}},
                {"key": key, "fields": {"kind": b"logout"}},
            ]
        ).redis_stream_write(ENDPOINT, key).run(realtime=True, cycles=1)

        stream = redis_stream_read(ENDPOINT, key).collect()
        stream.run(realtime=True, cycles=1)
        events = [e for tick in (stream.peek_value() or []) for e in tick]
        kinds = [e["fields"]["kind"] for e in events]
        self.assertIn(b"login", kinds)
        self.assertIn(b"logout", kinds)
        for e in events:
            self.assertIn("key", e)
            self.assertIn("id", e)
            self.assertIsInstance(e["fields"], dict)


if __name__ == "__main__":
    unittest.main()
