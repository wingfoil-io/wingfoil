"""Unit-level tests for the Python web adapter bindings that don't require
a running web server (use historical mode). These exercise WebServer
construction, codec handling, and the py_to_serde conversion path via
web_pub inside historical runs.
"""

import tempfile
import unittest

from wingfoil import WebServer, constant, ticker


class TestWebServerConstruction(unittest.TestCase):
    def test_historical_mode_no_port(self):
        server = WebServer("127.0.0.1:0", historical=True)
        self.assertEqual(server.port(), 0)

    def test_default_codec_is_bincode(self):
        server = WebServer("127.0.0.1:0", historical=True)
        self.assertEqual(server.codec_name(), "bincode")

    def test_json_codec(self):
        server = WebServer("127.0.0.1:0", codec="json", historical=True)
        self.assertEqual(server.codec_name(), "json")

    def test_bincode_codec_explicit(self):
        server = WebServer("127.0.0.1:0", codec="bincode", historical=True)
        self.assertEqual(server.codec_name(), "bincode")

    def test_invalid_codec_raises(self):
        with self.assertRaises(ValueError):
            WebServer("127.0.0.1:0", codec="not-a-codec", historical=True)

    def test_static_dir_accepted(self):
        with tempfile.TemporaryDirectory() as tmp:
            server = WebServer(
                "127.0.0.1:0", codec="json", static_dir=tmp, historical=True
            )
            self.assertEqual(server.port(), 0)

    def test_sub_returns_stream(self):
        # Subscribing in historical mode returns a PyStream (no frames arrive).
        server = WebServer("127.0.0.1:0", historical=True)
        sub_stream = server.sub("topic")
        # The stream can be collected (historical run just terminates cleanly).
        collected = sub_stream.collect()
        collected.run(realtime=False, cycles=1)


class TestWebPubConversions(unittest.TestCase):
    """Exercises the py_to_serde / serde_to_py conversion paths via
    web_pub. In historical mode no bytes hit the wire but the map
    closure still runs on each tick."""

    def _run_pub(self, value_fn, cycles=1):
        server = WebServer("127.0.0.1:0", codec="json", historical=True)
        node = ticker(0.01).count().map(value_fn).web_pub(server, "topic")
        node.run(realtime=False, cycles=cycles)

    def test_pub_none(self):
        self._run_pub(lambda _n: None)

    def test_pub_bool(self):
        self._run_pub(lambda n: bool(n % 2))

    def test_pub_int(self):
        self._run_pub(lambda n: n)

    def test_pub_large_int(self):
        # i64-fits, u64-fits, and very-large-int-falls-back-to-float paths.
        self._run_pub(lambda n: 2**60 + n)

    def test_pub_float(self):
        self._run_pub(lambda n: float(n) + 0.5)

    def test_pub_string(self):
        self._run_pub(lambda n: f"hello-{n}")

    def test_pub_bytes(self):
        self._run_pub(lambda n: bytes([n & 0xFF, 0xAB, 0xCD]))

    def test_pub_list(self):
        self._run_pub(lambda n: [n, n + 1, n + 2])

    def test_pub_dict(self):
        self._run_pub(lambda n: {"i": n, "f": 1.5, "s": "x"})

    def test_pub_nested(self):
        self._run_pub(
            lambda n: {
                "i": n,
                "arr": [1, 2.5, "s", True, None],
                "obj": {"k": "v", "nested": [1, 2]},
                "b": b"\xde\xad\xbe\xef",
            }
        )

    def test_pub_non_string_key_silently_becomes_null(self):
        # py_to_serde fails on non-string keys; the unwrap_or falls back
        # to Null so the node still runs without error.
        self._run_pub(lambda _n: {123: "int-key"})

    def test_pub_unsupported_type_silently_becomes_null(self):
        class Foo:
            pass

        self._run_pub(lambda _n: Foo())

    def test_pub_nan_falls_back_to_null(self):
        # Non-finite floats cannot be represented by serde_json::Number;
        # py_to_serde returns an error and unwrap_or turns it into Null.
        self._run_pub(lambda _n: float("nan"))

    def test_pub_constant(self):
        # Cover the case where web_pub wraps a constant (non-ticker) stream.
        server = WebServer("127.0.0.1:0", historical=True)
        node = constant({"hello": "world"}).web_pub(server, "topic")
        node.run(realtime=False, cycles=1)


if __name__ == "__main__":
    unittest.main()
