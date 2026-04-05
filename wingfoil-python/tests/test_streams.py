import unittest
from datetime import timedelta

from wingfoil import Graph, bimap, constant, ticker


class TestBasicOperators(unittest.TestCase):
    def test_map_and_collect(self):
        stream = (
            constant(1)
                .map(lambda x: x + 1)
                .sample(ticker(0.1))
                .collect()
        )
        stream.run(realtime=False, cycles=3)
        self.assertEqual(stream.peek_value(), [2, 2, 2])

    def test_filter(self):
        stream = (
            ticker(0.1)
                .count()
                .filter(lambda x: x % 2 == 0)
                .collect()
        )
        stream.run(realtime=False, cycles=7)
        self.assertEqual(stream.peek_value(), [2, 4, 6])

    def test_distinct(self):
        stream = (
            ticker(0.1)
                .count()
                .map(lambda x: x // 2)
                .distinct()
                .collect()
        )
        stream.run(realtime=False, cycles=5)
        self.assertEqual(stream.peek_value(), [0, 1, 2])

    def test_inspect(self):
        inspected_values = []
        stream = (
            ticker(0.1)
                .count()
                .inspect(lambda x: inspected_values.append(x))
                .collect()
        )
        stream.run(realtime=False, cycles=3)
        self.assertEqual(inspected_values, [1, 2, 3])
        self.assertEqual(stream.peek_value(), [1, 2, 3])

    def test_with_time(self):
        stream = (
            ticker(0.1)
                .count()
                .with_time()
                .collect()
        )
        stream.run(realtime=False, cycles=3)
        result = stream.peek_value()
        self.assertEqual(len(result), 3)
        times = [t for t, _ in result]
        values = [v for _, v in result]
        self.assertEqual(values, [1, 2, 3])
        self.assertTrue(all(times[i] < times[i + 1] for i in range(len(times) - 1)))

    def test_limit(self):
        stream = ticker(0.1).count().limit(3).collect()
        stream.run(realtime=False, cycles=10)
        self.assertEqual(stream.peek_value(), [1, 2, 3])

    def test_count_on_stream(self):
        # count() on a Stream (PyStream.count, not just PyNode.count)
        stream = ticker(0.1).count().count().collect()
        stream.run(realtime=False, cycles=4)
        self.assertEqual(stream.peek_value(), [1, 2, 3, 4])


class TestAggregations(unittest.TestCase):
    def test_sum(self):
        # sum accumulates f64 values; 1+2+3+4 = cumulative sums
        stream = ticker(0.1).count().map(lambda x: float(x)).sum().collect()
        stream.run(realtime=False, cycles=4)
        self.assertEqual(stream.peek_value(), [1.0, 3.0, 6.0, 10.0])

    def test_average(self):
        # running average of 1.0, 2.0, 3.0, 4.0
        stream = ticker(0.1).count().map(lambda x: float(x)).average().collect()
        stream.run(realtime=False, cycles=4)
        result = stream.peek_value()
        self.assertEqual(len(result), 4)
        self.assertAlmostEqual(result[0], 1.0)
        self.assertAlmostEqual(result[1], 1.5)
        self.assertAlmostEqual(result[3], 2.5)

    def test_buffer_tumbling_window(self):
        # buffer(3) emits a window every 3 ticks
        stream = ticker(0.1).count().buffer(3).collect()
        stream.run(realtime=False, cycles=7)
        result = stream.peek_value()
        self.assertEqual(result[0], [1, 2, 3])
        self.assertEqual(result[1], [4, 5, 6])
        self.assertEqual(result[2], [7])


class TestTransformations(unittest.TestCase):
    def test_difference(self):
        # count() increments by 1 each cycle; difference should always be 1
        stream = ticker(0.1).count().difference().collect()
        stream.run(realtime=False, cycles=5)
        result = stream.peek_value()
        self.assertTrue(len(result) > 0)
        self.assertTrue(all(v == 1 for v in result))

    def test_not(self):
        # not() is numeric negation via __neg__; not(-5) = 5
        stream = constant(-5)
        result = getattr(stream, 'not')().collect()
        result.run(realtime=False, cycles=1)
        self.assertEqual(result.peek_value(), [5])

    def test_logged(self):
        # logged() passes values through unchanged
        stream = ticker(0.1).count().logged("test-label").collect()
        stream.run(realtime=False, cycles=3)
        self.assertEqual(stream.peek_value(), [1, 2, 3])

    def test_delay(self):
        # delay with zero seconds passes values through
        stream = ticker(0.1).count().delay(0.0).collect()
        stream.run(realtime=False, cycles=3)
        self.assertEqual(stream.peek_value(), [1, 2, 3])


class TestSideEffects(unittest.TestCase):
    def test_for_each(self):
        received = []
        node = ticker(0.1).count().for_each(lambda val, t: received.append((val, t)))
        node.run(realtime=False, cycles=3)
        self.assertEqual(len(received), 3)
        vals = [v for v, _ in received]
        self.assertEqual(vals, [1, 2, 3])
        # time is passed as float (nanoseconds from epoch)
        times = [t for _, t in received]
        self.assertTrue(all(isinstance(t, float) for t in times))
        self.assertTrue(all(times[i] < times[i + 1] for i in range(len(times) - 1)))

    def test_finally(self):
        # finally_ is called once at shutdown with the last emitted value
        seen = []
        stream = ticker(0.1).count().collect()
        node = getattr(stream, 'finally')(lambda val: seen.append(val))
        node.run(realtime=False, cycles=3)
        self.assertEqual(len(seen), 1)
        self.assertEqual(seen[0], [1, 2, 3])


class TestSampling(unittest.TestCase):
    def test_sample_with_node_trigger(self):
        # sample(ticker) — ticker is a PyNode
        stream = constant(42).sample(ticker(0.1)).collect()
        stream.run(realtime=False, cycles=3)
        self.assertEqual(stream.peek_value(), [42, 42, 42])

    def test_sample_with_stream_trigger(self):
        # sample(stream) — trigger is a PyStream
        trigger_stream = ticker(0.1).count()
        stream = constant(7).sample(trigger_stream).collect()
        stream.run(realtime=False, cycles=3)
        self.assertEqual(stream.peek_value(), [7, 7, 7])


class TestBimap(unittest.TestCase):
    def test_bimap_add(self):
        a = ticker(0.1).count()
        b = ticker(0.1).count().map(lambda x: x * 10)
        result = bimap(a, b, lambda x, y: x + y).collect()
        result.run(realtime=False, cycles=3)
        self.assertEqual(result.peek_value(), [11, 22, 33])

    def test_bimap_string_concat(self):
        # constant fires once; bimap emits once on the first shared cycle
        a = constant("hello")
        b = constant(" world")
        result = bimap(a, b, lambda x, y: x + y).collect()
        result.run(realtime=False, cycles=1)
        self.assertEqual(result.peek_value(), ["hello world"])


class TestNodeAndGraph(unittest.TestCase):
    def test_node_run(self):
        # PyNode.run() directly (not stream.run())
        node = ticker(0.1)
        node.run(realtime=False, cycles=3)  # should not raise

    def test_node_count(self):
        node = ticker(0.1)
        count_stream = node.count().collect()
        count_stream.run(realtime=False, cycles=4)
        self.assertEqual(count_stream.peek_value(), [1, 2, 3, 4])

    def test_graph_with_multiple_streams(self):
        a = ticker(0.01).count().limit(3).collect()
        b = ticker(0.02).count().limit(2).collect()
        Graph([a, b]).run(realtime=False)
        self.assertEqual(a.peek_value(), [1, 2, 3])
        self.assertEqual(b.peek_value(), [1, 2])

    def test_graph_with_nodes(self):
        node = ticker(0.1)
        count = node.count().collect()
        Graph([node, count]).run(realtime=False, cycles=3)
        self.assertEqual(count.peek_value(), [1, 2, 3])

    def test_graph_mixed_streams_and_nodes(self):
        node = ticker(0.1)
        stream = node.count().collect()
        Graph([stream, node]).run(realtime=False, cycles=2)
        self.assertEqual(stream.peek_value(), [1, 2])

    def test_graph_invalid_type_raises(self):
        with self.assertRaises(Exception):
            Graph([42])


class TestRunModes(unittest.TestCase):
    def test_run_with_duration_float(self):
        stream = ticker(0.01).count().collect()
        stream.run(realtime=False, duration=0.05)
        self.assertGreater(len(stream.peek_value()), 0)

    def test_run_with_duration_timedelta(self):
        stream = ticker(0.01).count().collect()
        stream.run(realtime=False, duration=timedelta(seconds=0.05))
        self.assertGreater(len(stream.peek_value()), 0)

    def test_run_with_start_float(self):
        # start= sets the historical start timestamp (Unix seconds float)
        stream = ticker(0.01).count().collect()
        stream.run(realtime=False, start=0.0, cycles=3)
        self.assertEqual(stream.peek_value(), [1, 2, 3])

    def test_peek_value_before_run(self):
        stream = ticker(0.1).count()
        val = stream.peek_value()
        self.assertIsNone(val)


if __name__ == '__main__':
    unittest.main()
