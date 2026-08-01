"""Tests for the wingfoil-next OTLP Python bindings.

All of these run by default. `otlp_push` is a no-op under a historical run — the
Rust adapter's guard, so a backtest never publishes fast-forwarded values to a
live endpoint — which means the wiring and marshaling paths are exercisable
without any collector listening.

There is no `requires_otlp` tier: asserting an export arrived needs a collector
and a scrape of its output, which the Rust adapter's own integration tests
already cover end to end.
"""

import pytest

import wingfoil_next as wf

ENDPOINT = "http://127.0.0.1:4318"
SECOND_NANOS = 1_000_000_000


def test_module_exposes_otlp_push():
    assert callable(wf.otlp_push)


def test_push_constructs_a_terminal_stream():
    g = wf.Graph()
    values = g.values([1.5, 2.5], period_nanos=SECOND_NANOS)
    assert isinstance(wf.otlp_push(values, "m", ENDPOINT, "svc"), wf.Stream)


def test_a_historical_run_exports_nothing_and_succeeds():
    """The adapter's run-mode guard: no exporter is built, so no endpoint is hit."""
    g = wf.Graph()
    values = g.values([1.5, 2.5, 3.5], period_nanos=SECOND_NANOS)
    wf.otlp_push(values, "gauge", ENDPOINT, "svc")
    # Completes despite nothing listening on 4318.
    g.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)


def test_non_numeric_values_are_accepted_by_the_binding():
    """Stringification is the binding's job; parsing is the adapter's.

    A value that renders but is not a number records 0.0 with a warning rather
    than failing, matching the Rust adapter.
    """
    g = wf.Graph()
    values = g.values(["not-a-number"], period_nanos=SECOND_NANOS)
    wf.otlp_push(values, "gauge", ENDPOINT, "svc")
    g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)


def test_a_value_whose_str_raises_aborts_the_run():
    """Legacy turned this into an empty string, which published as 0.0."""

    class Exploding:
        def __str__(self):
            raise ValueError("boom")

    g = wf.Graph()
    # Realtime, so the sink is not short-circuited by the historical guard.
    values = g.values([Exploding()], period_nanos=SECOND_NANOS)
    wf.otlp_push(values, "gauge", ENDPOINT, "svc")
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=True, duration_nanos=2 * SECOND_NANOS)
    assert "str() on the stream value raised" in str(excinfo.value)
