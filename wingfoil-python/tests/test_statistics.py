"""Tests for the statistics Python bindings.

The statistics adapter is pure Rust with no external service, so — like augurs —
these tests run by default; there is no `requires_*` marker to gate. They pin
the pyo3 argument-marshaling glue (`Window` / `Weighting` / `EwmaSpan` and their
`int` / `str` / `float` shorthands) against deterministic historical-mode runs.

Every helper builds a *fresh* stream: node state (counters, rolling windows)
survives a run, so reusing one stream across runs would make later expectations
depend on earlier tests.
"""

import math
import unittest

from wingfoil import EwmaSpan, Weighting, Window, ticker


def _counts():
    """A fresh stream of floats 1.0, 2.0, 3.0, ... one per second of graph time."""
    return ticker(1.0).count().map(float)


def _run(stream, cycles):
    """Run `stream` in historical mode and return every value it emitted."""
    captured = stream.collect()
    captured.run(realtime=False, cycles=cycles)
    return captured.peek_value()


class TestMean(unittest.TestCase):
    def test_cumulative_default(self):
        # No window argument is the cumulative (unbounded) window.
        self.assertEqual([1.0, 1.5, 2.0, 2.5, 3.0], _run(_counts().mean(), 5))

    def test_explicit_unbounded_matches_default(self):
        self.assertEqual(
            _run(_counts().mean(), 5), _run(_counts().mean(Window.unbounded()), 5)
        )

    def test_count_window(self):
        # Rolling mean of the last three samples.
        self.assertEqual([1.0, 1.5, 2.0, 3.0, 4.0, 5.0], _run(_counts().mean(3), 6))

    def test_int_shorthand_matches_window_count(self):
        self.assertEqual(
            _run(_counts().mean(Window.count(3)), 6), _run(_counts().mean(3), 6)
        )

    def test_time_window(self):
        # Ticks land on whole seconds, and a sample exactly `seconds` old is
        # still in the window, so a 3s window holds four samples once warm.
        self.assertEqual(
            [1.0, 1.5, 2.0, 2.5, 3.5, 4.5], _run(_counts().mean(Window.seconds(3.0)), 6)
        )

    def test_time_weighting_differs_from_count(self):
        # Each sample is weighted by how long it was in effect, so the newest
        # sample (in effect for zero time so far) carries no weight yet.
        self.assertEqual(
            [1.0, 1.0, 1.5, 2.0, 2.5],
            _run(_counts().mean(Window.unbounded(), Weighting.Time), 5),
        )

    def test_weighting_accepts_strings(self):
        for name, weighting in (("count", Weighting.Count), ("time", Weighting.Time)):
            with self.subTest(weighting=name):
                self.assertEqual(
                    _run(_counts().mean(None, weighting), 5),
                    _run(_counts().mean(None, name), 5),
                )

    def test_average_is_cumulative_mean(self):
        self.assertEqual(_run(_counts().mean(), 5), _run(_counts().average(), 5))


class TestVarianceAndStd(unittest.TestCase):
    def test_cumulative_sample_variance(self):
        # ddof = 1, so the first tick yields 0.0 rather than a division by zero.
        expected = [0.0, 0.5, 1.0, 5.0 / 3.0, 2.5]
        for got, want in zip(_run(_counts().variance(), 5), expected):
            self.assertAlmostEqual(want, got)

    def test_std_is_sqrt_of_variance(self):
        variances = _run(_counts().variance(4), 8)
        stds = _run(_counts().std(4), 8)
        self.assertEqual(len(variances), len(stds))
        for variance, std in zip(variances, stds):
            self.assertAlmostEqual(math.sqrt(variance), std)

    def test_std_time_weighted_is_non_negative(self):
        # The time-weighted form is the population variance (no ddof), which is
        # a different number from the count-weighted one but never negative.
        stds = _run(_counts().std(Window.unbounded(), "time"), 6)
        self.assertTrue(all(std >= 0.0 for std in stds))
        self.assertGreater(stds[-1], 0.0)


class TestSumMinMax(unittest.TestCase):
    def test_sum_no_args_is_cumulative(self):
        # The pre-existing zero-argument `.sum()` keeps working unchanged.
        self.assertEqual([1.0, 3.0, 6.0, 10.0, 15.0], _run(_counts().sum(), 5))

    def test_sum_count_window(self):
        self.assertEqual([1.0, 3.0, 6.0, 9.0, 12.0, 15.0], _run(_counts().sum(3), 6))

    def test_sum_time_window(self):
        self.assertEqual(
            [1.0, 3.0, 6.0, 10.0, 14.0, 18.0],
            _run(_counts().sum(Window.seconds(3.0)), 6),
        )

    def test_cumulative_min_and_max(self):
        self.assertEqual([1.0] * 5, _run(_counts().min(), 5))
        self.assertEqual([1.0, 2.0, 3.0, 4.0, 5.0], _run(_counts().max(), 5))

    def test_rolling_min_and_max(self):
        self.assertEqual([1.0, 1.0, 1.0, 2.0, 3.0, 4.0], _run(_counts().min(3), 6))
        self.assertEqual([1.0, 2.0, 3.0, 4.0, 5.0, 6.0], _run(_counts().max(3), 6))

    def test_time_windowed_min(self):
        self.assertEqual(
            [1.0, 1.0, 1.0, 1.0, 2.0, 3.0], _run(_counts().min(Window.seconds(3.0)), 6)
        )


class TestMedian(unittest.TestCase):
    def test_count_window_even_and_odd(self):
        # Even window sizes interpolate between the two middle samples.
        self.assertEqual([1.0, 1.5, 2.0, 2.5, 3.5, 4.5], _run(_counts().median(4), 6))
        self.assertEqual([1.0, 1.5, 2.0, 3.0, 4.0, 5.0], _run(_counts().median(3), 6))

    def test_unbounded(self):
        self.assertEqual([1.0, 1.5, 2.0, 2.5, 3.0], _run(_counts().median(), 5))

    def test_time_weighted_differs(self):
        count_weighted = _run(_counts().median(5, Weighting.Count), 6)
        time_weighted = _run(_counts().median(5, Weighting.Time), 6)
        self.assertEqual(len(count_weighted), len(time_weighted))
        self.assertNotEqual(count_weighted, time_weighted)


class TestEwma(unittest.TestCase):
    def test_per_tick_float_shorthand(self):
        # The first sample seeds the average; thereafter
        # ewma = alpha * x + (1 - alpha) * previous.
        self.assertEqual([1.0, 1.5, 2.25, 3.125, 4.0625], _run(_counts().ewma(0.5), 5))

    def test_float_shorthand_matches_per_tick(self):
        self.assertEqual(
            _run(_counts().ewma(EwmaSpan.per_tick(0.5)), 5), _run(_counts().ewma(0.5), 5)
        )

    def test_alpha_one_is_passthrough(self):
        self.assertEqual([1.0, 2.0, 3.0, 4.0, 5.0], _run(_counts().ewma(1.0), 5))

    def test_half_life_seeds_then_lags(self):
        values = _run(_counts().ewma(EwmaSpan.half_life(2.0)), 5)
        self.assertEqual(1.0, values[0])
        # A decayed average trails a rising input but keeps rising with it.
        for i in range(1, len(values)):
            self.assertGreater(values[i], values[i - 1])
            self.assertLess(values[i], float(i + 1))


class TestArgumentTypes(unittest.TestCase):
    """`Window` / `Weighting` / `EwmaSpan` behave as ordinary Python values."""

    def test_window_repr(self):
        self.assertEqual("Window.count(3)", repr(Window.count(3)))
        self.assertEqual("Window.seconds(2.5)", repr(Window.seconds(2.5)))
        self.assertEqual("Window.unbounded()", repr(Window.unbounded()))

    def test_window_equality(self):
        self.assertEqual(Window.count(3), Window.count(3))
        self.assertNotEqual(Window.count(3), Window.count(4))
        self.assertNotEqual(Window.count(3), Window.unbounded())

    def test_ewma_span_repr_and_equality(self):
        self.assertEqual("EwmaSpan.per_tick(0.25)", repr(EwmaSpan.per_tick(0.25)))
        self.assertEqual(EwmaSpan.half_life(2.0), EwmaSpan.half_life(2.0))
        self.assertNotEqual(EwmaSpan.per_tick(0.25), EwmaSpan.half_life(2.0))

    def test_weighting_equality(self):
        self.assertEqual(Weighting.Count, Weighting.Count)
        self.assertNotEqual(Weighting.Count, Weighting.Time)


class TestValidation(unittest.TestCase):
    """Invalid arguments are rejected loudly rather than silently coerced."""

    def test_float_window_raises(self):
        # `mean(10)` (ten samples) and `mean(10.0)` (ten seconds?) would mean
        # wildly different things, so a bare float is rejected in favour of
        # Window.seconds(...).
        for op in ("mean", "variance", "std", "sum", "min", "max", "median"):
            with self.subTest(op=op):
                with self.assertRaises(TypeError):
                    getattr(_counts(), op)(3.0)

    def test_unknown_window_type_raises(self):
        with self.assertRaises(TypeError):
            _counts().mean("everything")

    def test_unknown_weighting_raises(self):
        with self.assertRaises(ValueError):
            _counts().mean(None, "exponential")

    def test_non_string_weighting_raises(self):
        with self.assertRaises(TypeError):
            _counts().mean(None, 1.5)

    def test_negative_and_non_finite_seconds_raise(self):
        for bad in (-1.0, float("nan"), float("inf")):
            with self.subTest(seconds=bad):
                with self.assertRaises(ValueError):
                    Window.seconds(bad)
                with self.assertRaises(ValueError):
                    EwmaSpan.half_life(bad)

    def test_zero_half_life_raises(self):
        with self.assertRaises(ValueError):
            EwmaSpan.half_life(0.0)

    def test_out_of_range_alpha_raises(self):
        # Rust only `debug_assert!`s this, so a release build would otherwise
        # produce a silently diverging average.
        for bad in (-0.1, 1.1, float("nan")):
            with self.subTest(alpha=bad):
                with self.assertRaises(ValueError):
                    EwmaSpan.per_tick(bad)
                with self.assertRaises(ValueError):
                    _counts().ewma(bad)

    def test_non_ewma_span_raises(self):
        with self.assertRaises(TypeError):
            _counts().ewma("fast")

    def test_non_float_input_fails_fast(self):
        # A non-numeric value is a hard error, not a silently substituted NaN
        # that would poison every downstream aggregate.
        bad = ticker(1.0).count().map(lambda n: "not a float")
        captured = bad.mean(3).collect()
        with self.assertRaises(Exception):
            captured.run(realtime=False, cycles=5)


if __name__ == "__main__":
    unittest.main()
