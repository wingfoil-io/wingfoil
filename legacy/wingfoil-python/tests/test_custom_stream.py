import unittest

from wingfoil import CustomStream, Graph, ticker


class DoubleStream(CustomStream):
    """A custom stream that doubles values from an upstream."""
    def __init__(self, upstream):
        super().__init__([upstream])

    def cycle(self) -> bool:
        val = self._upstreams[0].peek_value()
        self.set_value(val * 2)
        return True


class SumStream(CustomStream):
    """A custom stream that accumulates a running sum."""
    def __init__(self, upstream):
        super().__init__([upstream])
        self._total = 0

    def cycle(self) -> bool:
        val = self._upstreams[0].peek_value()
        self._total += val
        self.set_value(self._total)
        return True


class UnimplementedCycleStream(CustomStream):
    """A custom stream that does not implement cycle()."""
    def __init__(self, upstream):
        super().__init__([upstream])


class TestCustomStream(unittest.TestCase):

    def test_double_stream_doubles_values(self):
        src = ticker(0.1).count()
        doubled = DoubleStream(src).collect()
        doubled.run(realtime=False, cycles=4)
        self.assertEqual(doubled.peek_value(), [2, 4, 6, 8])

    def test_custom_stream_can_chain_operators(self):
        src = ticker(0.1).count()
        result = DoubleStream(src).filter(lambda x: x > 4).collect()
        result.run(realtime=False, cycles=5)
        self.assertEqual(result.peek_value(), [6, 8, 10])

    def test_custom_stream_running_sum(self):
        src = ticker(0.1).count()
        result = SumStream(src).collect()
        result.run(realtime=False, cycles=4)
        # sum of 1+2+3+4 = running totals: [1, 3, 6, 10]
        self.assertEqual(result.peek_value(), [1, 3, 6, 10])

    def test_custom_stream_upstreams_list(self):
        # Verify that the upstreams list is correctly stored
        src = ticker(0.1).count()
        # Access the inner Python object via a probe in __init__
        captured_upstreams = []

        class ProbeStream(CustomStream):
            def __init__(self, upstream):
                super().__init__([upstream])
                captured_upstreams.extend(self._upstreams)

            def cycle(self) -> bool:
                self.set_value(self._upstreams[0].peek_value())
                return True

        result = ProbeStream(src).collect()
        result.run(realtime=False, cycles=2)
        self.assertEqual(len(captured_upstreams), 1)
        self.assertEqual(result.peek_value(), [1, 2])

    def test_custom_stream_no_upstreams_init(self):
        # CustomStream() with no args initialises an empty upstreams list
        class EmptyUpstream(CustomStream):
            def __init__(self):
                super().__init__()  # no upstreams arg → _upstreams = []

            def cycle(self) -> bool:
                return False

        # Object is created successfully; _upstreams is empty
        # We can't run it as a source node, but construction must succeed
        # (CustomStream.__new__ returns a PyStream proxy)
        proxy = EmptyUpstream()
        # It is a PyStream proxy; peek_value returns None before any run
        self.assertIsNone(proxy.peek_value())

    def test_custom_stream_cycle_not_implemented_raises(self):
        src = ticker(0.1).count()
        result = UnimplementedCycleStream(src).collect()
        with self.assertRaises(Exception):
            result.run(realtime=False, cycles=1)

    def test_custom_stream_in_graph(self):
        src = ticker(0.1).count()
        doubled = DoubleStream(src)
        result = doubled.collect()
        Graph([result]).run(realtime=False, cycles=3)
        self.assertEqual(result.peek_value(), [2, 4, 6])


if __name__ == '__main__':
    unittest.main()
