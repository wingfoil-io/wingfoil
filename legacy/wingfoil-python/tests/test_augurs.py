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


def _sine(period=12):
    """A clean sine wave with the given period."""
    import math

    return ticker(1.0).count().map(lambda n: math.sin(n * math.tau / period))


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


class TestForecastMstl(unittest.TestCase):
    def test_mstl_forecast_swings(self):
        import math

        seasonal = ticker(1.0).count().map(
            lambda n: 0.1 * n + 5.0 * math.sin(n * math.tau / 12.0)
        )
        forecast = seasonal.augurs_forecast(120, 12, periods=[12])
        captured = forecast.collect()
        captured.run(realtime=False, cycles=80)
        point = captured.peek_value()[-1]["point"]
        self.assertEqual(len(point), 12)
        self.assertGreater(max(point) - min(point), 2.0)


class TestOutlierDbscan(unittest.TestCase):
    def test_dbscan_flags_diverging(self):
        outliers = _series().augurs_outlier(40, 0.5, detector="dbscan")
        captured = outliers.collect()
        captured.run(realtime=False, cycles=30)
        self.assertIn(3, captured.peek_value()[-1]["outlying"])


class TestChangepoint(unittest.TestCase):
    def test_detects_level_shift(self):
        series = ticker(1.0).count().map(lambda n: 50.0 if n > 20 else 0.0)
        changes = series.augurs_changepoint(60)
        captured = changes.collect()
        captured.run(realtime=False, cycles=50)
        indices = captured.peek_value()[-1]["indices"]
        self.assertTrue(indices, "expected a changepoint after the shift")
        self.assertTrue(any(i >= 10 for i in indices))

    def test_quiet_when_steady(self):
        import math

        series = ticker(1.0).count().map(lambda n: 10.0 + 0.1 * math.sin(n * 0.5))
        changes = series.augurs_changepoint(40)
        captured = changes.collect()
        captured.run(realtime=False, cycles=40)
        self.assertEqual(captured.peek_value()[-1]["indices"], [])

    def test_hazard_and_min_points_marshal(self):
        # Exercise the hazard/min_points optionals end to end: a level shift
        # is still detected with a non-default hazard and min_points.
        series = ticker(1.0).count().map(lambda n: 50.0 if n > 20 else 0.0)
        changes = series.augurs_changepoint(60, min_points=12, hazard=100.0)
        captured = changes.collect()
        captured.run(realtime=False, cycles=50)
        self.assertTrue(captured.peek_value()[-1]["indices"])


class TestSeasons(unittest.TestCase):
    def test_detects_period(self):
        seasons = _sine(12).augurs_seasons(96)
        captured = seasons.collect()
        captured.run(realtime=False, cycles=96)
        periods = captured.peek_value()[-1]["periods"]
        self.assertTrue(any(10 <= p <= 14 for p in periods), periods)

    def test_period_range_and_min_points_marshal(self):
        # Exercise the min_period/max_period/min_points optionals: a bracket
        # around 12 must still surface the period, and min_points must gate.
        seasons = _sine(12).augurs_seasons(
            96, min_points=40, min_period=8, max_period=16
        )
        captured = seasons.collect()
        captured.run(realtime=False, cycles=96)
        periods = captured.peek_value()[-1]["periods"]
        self.assertTrue(any(8 <= p <= 16 for p in periods), periods)

    def test_min_points_gates(self):
        seasons = _sine(12).augurs_seasons(96, min_points=50)
        captured = seasons.collect()
        captured.run(realtime=False, cycles=20)
        # 20 cycles < 50 min_points → never ticked.
        self.assertFalse(captured.peek_value())


class TestDtw(unittest.TestCase):
    def test_distance_matrix_shape_and_order(self):
        import math

        readings = ticker(1.0).count().map(
            lambda n: [
                math.sin(n * 0.3),
                math.sin(n * 0.3) + 0.02,
                5.0 * math.sin(n * 0.3) + 10.0,
            ]
        )
        dists = readings.augurs_dtw(30)
        captured = dists.collect()
        captured.run(realtime=False, cycles=30)
        rows = captured.peek_value()[-1]["rows"]
        self.assertEqual(len(rows), 3)
        self.assertEqual(len(rows[0]), 3)
        self.assertLess(rows[0][0], 1e-9)
        self.assertGreater(rows[0][2], rows[0][1])


class TestCluster(unittest.TestCase):
    def test_groups_series(self):
        import math

        def reading(n):
            low = math.sin(n * 0.3)
            high = math.sin(n * 0.3) + 20.0
            return [low, low + 0.02, high, high + 0.02, 50.0 * math.cos(n * 0.9) + 100.0]

        clusters = ticker(1.0).count().map(reading).augurs_cluster(30, 1.0, 2)
        captured = clusters.collect()
        captured.run(realtime=False, cycles=30)
        labels = captured.peek_value()[-1]["labels"]
        self.assertEqual(len(labels), 5)
        self.assertEqual(labels[0], labels[1])
        self.assertEqual(labels[2], labels[3])
        self.assertNotEqual(labels[0], labels[2])
        self.assertEqual(labels[4], -1)


class TestConstruction(unittest.TestCase):
    """Construction-only: exercise argument parsing without depending on output."""

    def test_forecast_constructs(self):
        stream = _ramp().augurs_forecast(32, 2)
        self.assertIsNotNone(stream)

    def test_forecast_mstl_constructs(self):
        stream = _ramp().augurs_forecast(48, 4, periods=[12])
        self.assertIsNotNone(stream)

    def test_outlier_constructs(self):
        stream = _series().augurs_outlier(16, 0.25)
        self.assertIsNotNone(stream)

    def test_new_ops_construct(self):
        self.assertIsNotNone(_ramp().augurs_changepoint(32))
        self.assertIsNotNone(_ramp().augurs_seasons(48))
        self.assertIsNotNone(_series().augurs_dtw(16))
        self.assertIsNotNone(_series().augurs_cluster(16, 1.0, 2))


class TestValidation(unittest.TestCase):
    """Invalid arguments are rejected loudly rather than silently coerced."""

    def test_unknown_detector_raises(self):
        with self.assertRaises(ValueError):
            _series().augurs_outlier(30, 0.5, detector="dbscann")

    def test_unknown_metric_raises(self):
        with self.assertRaises(ValueError):
            _series().augurs_dtw(30, metric="manhatten")

    def test_unknown_cluster_metric_raises(self):
        with self.assertRaises(ValueError):
            _series().augurs_cluster(30, 1.0, 2, metric="chebyshev")

    def test_out_of_range_sensitivity_raises(self):
        # A sensitivity outside (0, 1) raises ValueError at construction rather
        # than panicking across the FFI boundary.
        for bad in (0.0, 1.0, 1.5, -0.1):
            with self.assertRaises(ValueError):
                _series().augurs_outlier(30, bad)

    def test_out_of_range_level_raises(self):
        for bad in (0.0, 1.0, 1.5):
            with self.assertRaises(ValueError):
                _ramp().augurs_forecast(48, 2, level=bad)

    def test_invalid_mstl_period_raises(self):
        # STL cannot decompose a period below 2; caught at construction.
        for bad in ([1], [0], [12, 1]):
            with self.assertRaises(ValueError):
                _ramp().augurs_forecast(64, 4, periods=bad)

    def test_non_float_input_fails_fast(self):
        # A non-float value is a hard error (like .average()/.sum()), not a
        # silently substituted NaN that would poison the model window.
        bad = ticker(1.0).count().map(lambda n: "not a float")
        captured = bad.augurs_forecast(16, 1).collect()
        with self.assertRaises(Exception):
            captured.run(realtime=False, cycles=20)


if __name__ == "__main__":
    unittest.main()
