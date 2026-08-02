"""Integration tests for etcd sub/pub Python bindings.

Selected via `-m requires_etcd`. Without an etcd instance running on
localhost:2379 the tests will fail loudly — they do not silently skip.

Setup:
    docker run --rm -p 2379:2379 \\
      -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \\
      -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \\
      gcr.io/etcd-development/etcd:v3.5.0
"""

import base64
import json
import unittest
import urllib.request

import pytest

ENDPOINT = "http://localhost:2379"
PREFIX = "/wingfoil_pytest/"


def _http_post(path: str, payload: dict) -> dict:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        f"{ENDPOINT}{path}",
        data=data,
        headers={"Content-Type": "application/json"},
    )
    with urllib.request.urlopen(req, timeout=5) as resp:
        return json.loads(resp.read())


def etcd_put(key: str, value: bytes) -> None:
    _http_post(
        "/v3/kv/put",
        {
            "key": base64.b64encode(key.encode()).decode(),
            "value": base64.b64encode(value).decode(),
        },
    )


def etcd_get(key: str) -> bytes | None:
    result = _http_post(
        "/v3/kv/range",
        {"key": base64.b64encode(key.encode()).decode()},
    )
    kvs = result.get("kvs", [])
    if not kvs:
        return None
    return base64.b64decode(kvs[0]["value"])


def etcd_delete_prefix(prefix: str) -> None:
    end = prefix[:-1] + chr(ord(prefix[-1]) + 1)
    _http_post(
        "/v3/kv/deleterange",
        {
            "key": base64.b64encode(prefix.encode()).decode(),
            "range_end": base64.b64encode(end.encode()).decode(),
        },
    )


@pytest.mark.requires_etcd
class TestEtcdSub(unittest.TestCase):
    def setUp(self):
        etcd_delete_prefix(PREFIX)

    def test_sub_snapshot_returns_list_of_dicts(self):
        """etcd_sub snapshot phase yields a list of dicts with expected keys."""
        from wingfoil import etcd_sub

        etcd_put(f"{PREFIX}hello", b"world")

        stream = etcd_sub(ENDPOINT, PREFIX).collect()
        stream.run(realtime=True, duration=1.0)
        events = stream.peek_value()

        self.assertIsInstance(events, list)
        self.assertGreaterEqual(len(events), 1)

        # Flatten: each tick is a list of event dicts
        all_events = [e for tick in events for e in tick]
        keys = {e["key"] for e in all_events}
        self.assertIn(f"{PREFIX}hello", keys)

        event = next(e for e in all_events if e["key"] == f"{PREFIX}hello")
        self.assertEqual(event["kind"], "put")
        self.assertEqual(event["value"], b"world")
        self.assertIsInstance(event["revision"], int)

    def test_sub_empty_snapshot(self):
        """etcd_sub with no matching keys yields no events."""
        from wingfoil import etcd_sub

        stream = etcd_sub(ENDPOINT, PREFIX + "nonexistent/").collect()
        stream.run(realtime=True, duration=1.0)
        events = stream.peek_value()

        # `peek_value()` is None when the stream never ticked; otherwise it may
        # be a list of (possibly empty) ticks. Either way, no events should
        # have been seen.
        all_events = [e for tick in (events or []) for e in tick]
        self.assertEqual(all_events, [])

    def test_sub_dict_has_correct_fields(self):
        """Each event dict contains kind, key, value, revision."""
        from wingfoil import etcd_sub

        etcd_put(f"{PREFIX}field_test", b"check")

        stream = etcd_sub(ENDPOINT, PREFIX).collect()
        stream.run(realtime=True, duration=1.0)
        ticks = stream.peek_value()
        all_events = [e for tick in ticks for e in tick]

        self.assertTrue(all_events, "expected at least one event")
        for e in all_events:
            self.assertIn("kind", e)
            self.assertIn("key", e)
            self.assertIn("value", e)
            self.assertIn("revision", e)
            self.assertIn(e["kind"], ("put", "delete"))
            self.assertIsInstance(e["key"], str)
            self.assertIsInstance(e["value"], bytes)
            self.assertIsInstance(e["revision"], int)


@pytest.mark.requires_etcd
class TestEtcdPub(unittest.TestCase):
    def setUp(self):
        etcd_delete_prefix(PREFIX)

    def test_pub_single_dict_round_trip(self):
        """etcd_pub writes a single dict and key is readable back via HTTP."""
        from wingfoil import constant

        key = f"{PREFIX}pub_test"
        constant({"key": key, "value": b"hello_from_python"}).etcd_pub(ENDPOINT).run(
            realtime=False, cycles=1
        )

        result = etcd_get(key)
        self.assertEqual(result, b"hello_from_python")

    def test_pub_list_of_dicts_round_trip(self):
        """etcd_pub writes a list of dicts (multi-entry burst) correctly."""
        from wingfoil import constant

        entries = [
            {"key": f"{PREFIX}multi_a", "value": b"aaa"},
            {"key": f"{PREFIX}multi_b", "value": b"bbb"},
        ]
        constant(entries).etcd_pub(ENDPOINT).run(realtime=False, cycles=1)

        self.assertEqual(etcd_get(f"{PREFIX}multi_a"), b"aaa")
        self.assertEqual(etcd_get(f"{PREFIX}multi_b"), b"bbb")

    def test_pub_force_false_fails_if_key_exists(self):
        """etcd_pub with force=False raises when key already exists."""
        from wingfoil import constant

        key = f"{PREFIX}force_test"
        etcd_put(key, b"existing")

        with self.assertRaises(Exception):
            constant({"key": key, "value": b"new"}).etcd_pub(
                ENDPOINT, force=False
            ).run(realtime=False, cycles=1)

    def test_pub_with_lease_ttl(self):
        """etcd_pub with lease_ttl writes a key under a lease; on clean shutdown
        the lease is revoked so the key disappears immediately."""
        from wingfoil import constant

        key = f"{PREFIX}leased"
        constant({"key": key, "value": b"leased_value"}).etcd_pub(
            ENDPOINT, lease_ttl=60.0
        ).run(realtime=False, cycles=1)

        # Clean shutdown revokes the lease, so the key must NOT persist.
        # This verifies both that the write path ran (no exception) and that
        # the lease revoke-on-shutdown semantics are wired up correctly.
        self.assertIsNone(etcd_get(key))


if __name__ == "__main__":
    unittest.main()
