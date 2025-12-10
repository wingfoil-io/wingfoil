import unittest
from wingfoil import constant, ticker

class TestStreams(unittest.TestCase):
    def test_map_and_collect(self):
        # Create a constant stream [1, 1, 1], map to [2, 2, 2], and collect.
        stream = (
            constant(1)
            .limit(3)
            .map(lambda x: x + 1)
            .collect()
        )
        
        stream.run(realtime=False)
        self.assertEqual(stream.peek_value(), [2, 2, 2])

    def test_filter(self):
        # Stream 0..4, keep evens -> [0, 2, 4]
        stream = (
            ticker(0.1)
            .count()
            .limit(5)
            .filter(lambda x: x % 2 == 0)
            .collect()
        )
        
        stream.run(realtime=False)
        self.assertEqual(stream.peek_value(), [0, 2, 4])

    def test_distinct(self):
        # Generate duplicates: 0,0, 1,1, 2,2 -> distinct -> 0, 1, 2
        stream = (
            ticker(0.1)
            .count()
            .limit(6)
            .map(lambda x: x // 2)
            .distinct()
            .collect()
        )
        
        stream.run(realtime=False)
        self.assertEqual(stream.peek_value(), [0, 1, 2])

if __name__ == '__main__':
    unittest.main()
