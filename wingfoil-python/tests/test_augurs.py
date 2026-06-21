"""Tests for the augurs Python bindings (stream.augurs_forecast / augurs_outlier).

augurs is a pure-Rust compute library with no external service, so these tests
run by default — there is no `requires_*` marker to gate. They exercise the
pyo3 marshaling glue end to end against deterministic historical-mode runs.
"""

import unittest

from wingfoil import ticker


def _ramp():
    """A rising stream of floats: n + sin(n/2)."""
    import math

    return ticker(1.0).count().map(lambda n: float(n) + math.sin(n * 0.5))


def _series():
    """Four readings per tick; series 3 diverges after tick 15."""
    import math

    def reading(n):
        base = 100.0 + math.sin(n * 0.4)
        diverging = base + 80.0 if n > 15 else base + 0.2
        return [base, base + 0.1, base - 0.1, diverging]

    return ticker(1.0).count().map(reading)


class TestForecast(unittest.TestCase):
    def test_forecast_shape(self):
        forecast = _ramp().augurs_forecast(48, 3)
        captured = forecast.collect()
        captured.run(realtime=False, cycles=30)
        last = captured.peek_value()[-1]
        self.assertIsInstance(last, dict)
        self.assertEqual(len(last["point"]), 3)

    def test_forecast_with_intervals(self):
        forecast = _ramp().augurs_forecast(48, 2, level=0.9)
        captured = forecast.collect()
        captured.run(realtime=False, cycles=30)
        last = captured.peek_value()[-1]
        self.assertEqual(len(last["lower"]), 2)
        self.assertEqual(len(last["upper"]), 2)
        for i in range(2):
            self.assertLessEqual(last["lower"][i], last["point"][i])
            self.assertGreaterEqual(last["upper"][i], last["point"][i])

    def test_forecast_waits_for_min_points(self):
        forecast = _ramp().augurs_forecast(64, 1, min_points=20)
        captured = forecast.collect()
        captured.run(realtime=False, cycles=10)
        # 10 cycles < 20 min_points → never ticked, so the collected stream
        # has no value (peeks as None rather than an empty list).
        self.assertFalse(captured.peek_value())


class TestOutlier(unittest.TestCase):
    def test_flags_diverging_series(self):
        outliers = _series().augurs_outlier(30, 0.5)
        captured = outliers.collect()
        captured.run(realtime=False, cycles=30)
        last = captured.peek_value()[-1]
        self.assertIn(3, last["outlying"])
        self.assertEqual(len(last["scores"]), 4)

    def test_aligned_series_quiet(self):
        aligned = ticker(1.0).count().map(lambda n: [50.0, 50.05, 49.95])
        outliers = aligned.augurs_outlier(20, 0.5)
        captured = outliers.collect()
        captured.run(realtime=False, cycles=20)
        self.assertEqual(captured.peek_value()[-1]["outlying"], [])


class TestConstruction(unittest.TestCase):
    """Construction-only: exercise argument parsing without depending on output."""

    def test_forecast_constructs(self):
        stream = _ramp().augurs_forecast(32, 2)
        self.assertIsNotNone(stream)

    def test_outlier_constructs(self):
        stream = _series().augurs_outlier(16, 0.25)
        self.assertIsNotNone(stream)


if __name__ == "__main__":
    unittest.main()
