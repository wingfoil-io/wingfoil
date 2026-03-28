import unittest

from wingfoil import constant, ticker

class TestStreams(unittest.TestCase):
    def test_map_and_collect(self):
        stream = (
            constant(1)
                .map(lambda x: x + 1)
                .sample(ticker(0.1))
                .collect()
        )
        stream.run(realtime=False, cycles = 3)
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
        stream.run(realtime=False, cycles = 5)
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
        self.assertEqual(inspected_values, [1, 2, 3])        # lambda was called
        self.assertEqual(stream.peek_value(), [1, 2, 3])     # values passed through unchanged

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
        # times should be monotonically increasing
        self.assertTrue(all(times[i] < times[i + 1] for i in range(len(times) - 1)))

if __name__ == '__main__':
    unittest.main()
