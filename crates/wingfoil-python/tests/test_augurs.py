"""Tests for the wingfoil augurs Python bindings.

All of these run by default — augurs is pure compute with no service behind it,
so the analytics run for real here rather than behind a marker.
"""

import math

import pytest

import wingfoil as wf

SECOND_NANOS = 1_000_000_000


def run_over(values, wire, cycles=None):
    """Wire `wire(stream)` over `values` and collect every tick it produced."""
    g = wf.Graph()
    source = g.values(values, period_nanos=SECOND_NANOS)
    seen = []
    wire(source).inspect(seen.append)
    g.run(
        realtime=False,
        start_nanos=0,
        duration_nanos=(len(values) + 2) * SECOND_NANOS,
    )
    return seen


def test_module_exposes_the_augurs_surface():
    for name in (
        "augurs_forecast",
        "augurs_changepoint",
        "augurs_seasons",
        "augurs_outlier",
        "augurs_dtw",
        "augurs_cluster",
    ):
        assert callable(getattr(wf, name)), name


# ---- Single-series ops (float input) ----


def test_forecast_yields_point_lower_upper():
    series = [float(i) for i in range(1, 41)]
    seen = run_over(series, lambda s: wf.augurs_forecast(s, window=32, horizon=3))
    assert seen, "expected at least one forecast once min_points accumulated"
    last = seen[-1]
    assert {"point", "lower", "upper"} == set(last)
    assert 3 == len(last["point"])
    assert all(math.isfinite(v) for v in last["point"])
    # No level requested, so the interval bounds stay empty.
    assert [] == last["lower"] and [] == last["upper"]


def test_forecast_with_a_level_populates_the_interval():
    series = [float(i) for i in range(1, 41)]
    seen = run_over(
        series, lambda s: wf.augurs_forecast(s, window=32, horizon=2, level=0.95)
    )
    last = seen[-1]
    assert 2 == len(last["lower"]) and 2 == len(last["upper"])
    assert all(lo <= up for lo, up in zip(last["lower"], last["upper"]))


def test_min_points_gates_when_forecasting_starts():
    """`min_points` is how many samples must accumulate before the op ticks at
    all — one of the knobs legacy never exposed."""
    series = [float(i) for i in range(1, 41)]

    def ticks(min_points):
        return len(
            run_over(
                series,
                lambda s: wf.augurs_forecast(
                    s, window=32, horizon=1, min_points=min_points
                ),
            )
        )

    assert 21 == ticks(20)  # forecasts from the 20th sample on: 40 - 20 + 1
    assert 11 == ticks(30)
    assert 0 == ticks(100)  # a gate the series never reaches: never ticks


def test_forecast_mstl_requires_periods():
    g = wf.Graph()
    source = g.values([1.0], period_nanos=SECOND_NANOS)
    with pytest.raises(RuntimeError) as excinfo:
        wf.augurs_forecast(source, window=32, horizon=2, model="mstl")
    assert "needs periods" in str(excinfo.value)


def test_forecast_rejects_an_unknown_model():
    g = wf.Graph()
    source = g.values([1.0], period_nanos=SECOND_NANOS)
    with pytest.raises(RuntimeError) as excinfo:
        wf.augurs_forecast(source, window=8, horizon=1, model="arima")
    assert "unknown model 'arima'" in str(excinfo.value)


def test_changepoint_finds_a_level_shift():
    # Flat, then a large step: the shift should be reported at some point.
    series = [1.0] * 20 + [50.0] * 20
    seen = run_over(series, lambda s: wf.augurs_changepoint(s, window=40))
    assert seen
    assert all("indices" in tick for tick in seen)
    assert any(tick["indices"] for tick in seen), "expected a changepoint"


def test_seasons_yields_periods():
    # Repeating 8-sample cycle.
    series = [float(i % 8) for i in range(64)]
    seen = run_over(series, lambda s: wf.augurs_seasons(s, window=64))
    assert seen
    assert all("periods" in tick for tick in seen)


def test_a_non_float_tick_aborts_the_run():
    """Legacy substituted 0.0 here, silently feeding the model bad data."""
    g = wf.Graph()
    source = g.values(["not-a-number"], period_nanos=SECOND_NANOS)
    wf.augurs_changepoint(source, window=4).inspect(lambda _: None)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)
    assert "must be a float" in str(excinfo.value)


# ---- Multi-series ops (list input) ----


def test_outlier_scores_every_series():
    # Three series; the third diverges sharply.
    ticks = [[float(i), float(i) + 0.1, float(i) * 5.0] for i in range(1, 33)]
    seen = run_over(ticks, lambda s: wf.augurs_outlier(s, window=32, sensitivity=0.5))
    assert seen
    last = seen[-1]
    assert {"outlying", "scores"} == set(last)
    assert 3 == len(last["scores"])


def test_outlier_rejects_an_unknown_detector():
    g = wf.Graph()
    source = g.values([[1.0]], period_nanos=SECOND_NANOS)
    with pytest.raises(RuntimeError) as excinfo:
        wf.augurs_outlier(source, window=8, sensitivity=0.5, detector="iqr")
    assert "unknown detector 'iqr'" in str(excinfo.value)


def test_outlier_rejects_a_sensitivity_outside_the_unit_interval():
    """Sensitivity must be strictly between 0 and 1. Legacy raised at
    construction; next builds the detector per cycle, so it aborts the run."""
    g = wf.Graph()
    ticks = [[float(i), float(i) * 3.0] for i in range(6)]
    source = g.values(ticks, period_nanos=SECOND_NANOS)
    wf.augurs_outlier(source, window=4, sensitivity=1.5).inspect(lambda _: None)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)
    assert "invalid sensitivity" in str(excinfo.value)


def test_dtw_yields_a_square_matrix():
    ticks = [[float(i), float(i) * 2.0] for i in range(1, 25)]
    seen = run_over(ticks, lambda s: wf.augurs_dtw(s, window=16))
    assert seen
    rows = seen[-1]["rows"]
    assert 2 == len(rows) and all(2 == len(r) for r in rows)
    # A series is zero distance from itself, and the matrix is symmetric.
    assert 0.0 == rows[0][0] and 0.0 == rows[1][1]
    assert rows[0][1] == pytest.approx(rows[1][0])


def test_cluster_labels_every_series():
    ticks = [[float(i), float(i) + 0.05, float(i) * 10.0] for i in range(1, 25)]
    seen = run_over(
        ticks,
        lambda s: wf.augurs_cluster(s, window=16, epsilon=5.0, min_cluster_size=2),
    )
    assert seen
    labels = seen[-1]["labels"]
    assert 3 == len(labels)
    assert all(isinstance(v, int) for v in labels)


def test_a_bare_float_is_not_a_series():
    """Legacy turned this into an empty series rather than an error."""
    g = wf.Graph()
    source = g.values([1.0], period_nanos=SECOND_NANOS)
    wf.augurs_dtw(source, window=4).inspect(lambda _: None)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)
    assert "must be a list of floats" in str(excinfo.value)


def test_dtw_rejects_an_unknown_metric():
    g = wf.Graph()
    source = g.values([[1.0]], period_nanos=SECOND_NANOS)
    with pytest.raises(RuntimeError) as excinfo:
        wf.augurs_dtw(source, window=8, metric="cosine")
    assert "unknown metric 'cosine'" in str(excinfo.value)
