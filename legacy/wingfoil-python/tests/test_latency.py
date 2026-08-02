"""Tests for the Python latency bindings (Latency, TracedBytes, stamp, latency_report)."""

import unittest

from wingfoil import Graph, Latency, TracedBytes, ticker


class TestLatency(unittest.TestCase):
    def test_new_and_getitem_setitem(self):
        lat = Latency(["a", "b", "c"])
        self.assertEqual(lat.stages, ["a", "b", "c"])
        self.assertEqual(lat.stamps, [0, 0, 0])
        self.assertEqual(len(lat), 3)

        lat["a"] = 100
        lat["b"] = 200
        self.assertEqual(lat["a"], 100)
        self.assertEqual(lat["b"], 200)
        self.assertEqual(lat["c"], 0)

    def test_repr(self):
        lat = Latency(["a", "b"])
        lat["a"] = 7
        lat["b"] = 9
        r = repr(lat)
        self.assertIn("Latency(", r)
        self.assertIn("a=7", r)
        self.assertIn("b=9", r)

    def test_empty_stages_raises(self):
        with self.assertRaises(ValueError):
            Latency([])

    def test_duplicate_stages_raises(self):
        with self.assertRaises(ValueError):
            Latency(["a", "a"])

    def test_unknown_stage_getitem_raises(self):
        lat = Latency(["a"])
        with self.assertRaises(KeyError):
            _ = lat["missing"]

    def test_unknown_stage_setitem_raises(self):
        lat = Latency(["a"])
        with self.assertRaises(KeyError):
            lat["missing"] = 5

    def test_to_bytes_and_from_bytes_roundtrip(self):
        lat = Latency(["a", "b"])
        lat["a"] = 12345
        lat["b"] = 67890
        data = lat.to_bytes()
        self.assertIsInstance(data, bytes)
        self.assertEqual(len(data), 16)  # 2 stages * 8 bytes

        restored = Latency.from_bytes(data, ["a", "b"])
        self.assertEqual(restored.stamps, [12345, 67890])
        self.assertEqual(restored["a"], 12345)

    def test_from_bytes_too_short_raises(self):
        with self.assertRaises(ValueError):
            Latency.from_bytes(b"\x00" * 4, ["a", "b"])


class TestTracedBytes(unittest.TestCase):
    def test_traced_bytes_fields(self):
        lat = Latency(["s"])
        tb = TracedBytes(b"hello", lat)
        self.assertEqual(tb.payload, b"hello")
        # latency is a Python-accessible Latency object
        self.assertEqual(tb.latency.stages, ["s"])

    def test_traced_bytes_repr(self):
        lat = Latency(["s1", "s2"])
        lat["s1"] = 10
        tb = TracedBytes(b"x", lat)
        r = repr(tb)
        self.assertIn("TracedBytes", r)
        self.assertIn("Latency(", r)


class TestStamp(unittest.TestCase):
    def _traced(self, _n):
        return TracedBytes(b"payload", Latency(["start", "mid", "end"]))

    def test_stamp_records_wall_time(self):
        received = []

        stream = (
            ticker(0.01)
            .count()
            .map(self._traced)
            .stamp("start")
            .stamp("mid")
            .stamp("end")
        )
        node = stream.for_each(lambda tb, _t: received.append(tb.latency.stamps[:]))
        node.run(realtime=False, cycles=3)

        self.assertEqual(len(received), 3)
        for stamps in received:
            # All three stages were stamped (non-zero)
            self.assertTrue(all(s > 0 for s in stamps))
            # Monotonic within a single tick
            self.assertLessEqual(stamps[0], stamps[1])
            self.assertLessEqual(stamps[1], stamps[2])

    def test_stamp_precise_also_stamps(self):
        received = []
        stream = (
            ticker(0.01)
            .count()
            .map(self._traced)
            .stamp_precise("start")
            .stamp_precise("mid")
            .stamp_precise("end")
        )
        node = stream.for_each(lambda tb, _t: received.append(tb.latency.stamps[:]))
        node.run(realtime=False, cycles=2)
        self.assertEqual(len(received), 2)
        for stamps in received:
            self.assertTrue(all(s > 0 for s in stamps))

    def test_stamp_if_enabled_false_is_noop(self):
        # When disabled, stamp_if / stamp_precise_if pass the stream through
        # unchanged (no stamping happens).
        stream = (
            ticker(0.01)
            .count()
            .map(self._traced)
            .stamp_if("start", False)
            .stamp_precise_if("mid", False)
        )
        received = []
        node = stream.for_each(lambda tb, _t: received.append(tb.latency.stamps[:]))
        node.run(realtime=False, cycles=2)
        # No stage was stamped, so all zero.
        for stamps in received:
            self.assertEqual(stamps, [0, 0, 0])

    def test_stamp_if_enabled_true_stamps(self):
        stream = (
            ticker(0.01)
            .count()
            .map(self._traced)
            .stamp_if("start", True)
            .stamp_precise_if("end", True)
        )
        received = []
        node = stream.for_each(lambda tb, _t: received.append(tb.latency.stamps[:]))
        node.run(realtime=False, cycles=2)
        for stamps in received:
            self.assertGreater(stamps[0], 0)  # start stamped
            self.assertEqual(stamps[1], 0)  # mid untouched
            self.assertGreater(stamps[2], 0)  # end stamped

    def test_stamp_on_non_traced_raises(self):
        # stamp() on a non-TracedBytes stream should fail at runtime.
        stream = ticker(0.01).count().stamp("stage")
        node = stream.for_each(lambda _v, _t: None)
        with self.assertRaises(Exception):
            node.run(realtime=False, cycles=1)


class TestLatencyReport(unittest.TestCase):
    def _traced(self, _n):
        return TracedBytes(b"payload", Latency(["a", "b", "c"]))

    def test_latency_report_runs(self):
        stream = (
            ticker(0.01)
            .count()
            .map(self._traced)
            .stamp("a")
            .stamp("b")
            .stamp("c")
        )
        node = stream.latency_report(["a", "b", "c"], print_on_teardown=False)
        node.run(realtime=False, cycles=5)  # no exception => pass

    def test_latency_report_print_on_teardown(self):
        stream = (
            ticker(0.01)
            .count()
            .map(self._traced)
            .stamp("a")
            .stamp("b")
            .stamp("c")
        )
        node = stream.latency_report(["a", "b", "c"], print_on_teardown=True)
        node.run(realtime=False, cycles=3)  # prints report, no exception

    def test_latency_report_if_disabled(self):
        # With enabled=False, no report sink is installed and the node runs clean.
        stream = ticker(0.01).count()
        node = stream.latency_report_if(["a", "b"], enabled=False)
        node.run(realtime=False, cycles=2)

    def test_latency_report_if_enabled(self):
        stream = (
            ticker(0.01)
            .count()
            .map(self._traced)
            .stamp("a")
            .stamp("b")
            .stamp("c")
        )
        node = stream.latency_report_if(
            ["a", "b", "c"], enabled=True, print_on_teardown=False
        )
        node.run(realtime=False, cycles=3)

    def test_latency_report_on_non_traced_raises(self):
        stream = ticker(0.01).count()
        node = stream.latency_report(["a", "b"], print_on_teardown=False)
        with self.assertRaises(Exception):
            node.run(realtime=False, cycles=1)


if __name__ == "__main__":
    unittest.main()
