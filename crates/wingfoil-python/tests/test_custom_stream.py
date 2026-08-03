"""`CustomStream` — the subclass form of a Python-defined graph node.

The composition form (`Graph.custom_node`) is covered in `test_interop.py`;
these cover the pure-Python base class layered on it, including the legacy
shape it exists to preserve and the two deviations from legacy it cannot.
"""

import math

import pytest

import wingfoil as wf


class Doubler(wf.CustomStream):
    def cycle(self):
        (src,) = self.upstreams()
        self.set_value(src.peek_value() * 2)
        return True


def test_subclass_wires_and_ticks():
    g = wf.Graph()
    out = Doubler(g, [g.counter(period_nanos=100)]).collect()
    g.run(cycles=4)
    assert [(0, 2), (100, 4), (200, 6), (300, 8)] == out.value()


def test_constructor_returns_a_stream_that_chains():
    g = wf.Graph()
    node = Doubler(g, [g.counter(period_nanos=100)])
    # Not the subclass instance — the wired Stream, as in legacy.
    assert isinstance(node, wf.Stream)
    out = node.map(lambda v: v + 1).collect()
    g.run(cycles=3)
    assert [(0, 3), (100, 5), (200, 7)] == out.value()


def test_upstreams_are_ordered_and_independent():
    """Legacy's example shape: combine upstreams as base-10 digits."""

    class Digits(wf.CustomStream):
        def cycle(self):
            value = 0
            for i, src in enumerate(self.upstreams()):
                value += src.peek_value() * math.pow(10, i)
            self.set_value(value)
            return True

        def peek(self):  # exercise that an override still reaches the engine
            return super().peek()

    g = wf.Graph()
    ones = g.counter(period_nanos=100)
    tens = g.counter(period_nanos=100).map(lambda n: n + 1)
    out = Digits(g, [ones, tens]).collect()
    g.run(cycles=3)
    # n + 10*(n + 1) == 11n + 10
    assert [(0, 21.0), (100, 32.0), (200, 43.0)] == out.value()


def test_subclass_can_stay_quiet():
    class Evens(wf.CustomStream):
        def cycle(self):
            (src,) = self.upstreams()
            value = src.peek_value()
            if value % 2:
                return False
            self.set_value(value)
            return True

    g = wf.Graph()
    out = Evens(g, [g.counter(period_nanos=100)]).collect()
    g.run(cycles=5)
    # Odd cycles return False: no tick, so no entry — and no stale repeat.
    assert [(100, 2), (300, 4)] == out.value()


def test_subclass_init_receives_remaining_args():
    class Scaled(wf.CustomStream):
        def __init__(self, factor):
            self.factor = factor

        def cycle(self):
            (src,) = self.upstreams()
            self.set_value(src.peek_value() * self.factor)
            return True

    g = wf.Graph()
    out = Scaled(g, [g.counter(period_nanos=100)], 10).collect()
    g.run(cycles=3)
    assert [(0, 10), (100, 20), (200, 30)] == out.value()


def test_init_may_seed_a_value_before_wiring():
    class Seeded(wf.CustomStream):
        def __init__(self):
            self.set_value(-1.0)  # state exists before the first cycle

        def cycle(self):
            (src,) = self.upstreams()
            self.set_value(self.peek() + src.peek_value())
            return True

    g = wf.Graph()
    out = Seeded(g, [g.counter(period_nanos=100)]).collect()
    g.run(cycles=3)
    # -1 + 1, then 0 + 2, then 2 + 3.
    assert [(0, 0.0), (100, 2.0), (200, 5.0)] == out.value()


def test_missing_cycle_raises_not_implemented():
    class Incomplete(wf.CustomStream):
        pass

    g = wf.Graph()
    Incomplete(g, [g.counter(period_nanos=100)])
    with pytest.raises(Exception, match="must implement cycle"):
        g.run(cycles=1)


def test_exception_in_cycle_aborts_the_run():
    class Boom(wf.CustomStream):
        def cycle(self):
            raise ValueError("boom")

    g = wf.Graph()
    Boom(g, [g.counter(period_nanos=100)])
    with pytest.raises(Exception, match="boom"):
        g.run(cycles=1)


def test_upstream_value_reads_none_before_a_tick():
    """A not-yet-ticked upstream is `None`, not a missing entry."""
    seen = []

    class Watcher(wf.CustomStream):
        def cycle(self):
            seen.append([u.peek_value() for u in self.upstreams()])
            self.set_value(0.0)
            return True

    g = wf.Graph()
    fast = g.counter(period_nanos=100)
    slow = fast.filter_value(lambda n: n >= 3)
    Watcher(g, [fast, slow])
    g.run(cycles=3)
    assert [[1, None], [2, None], [3, 3]] == seen


def test_graph_with_a_custom_node_is_single_run():
    """Caller-owned Python state has no engine reset hook, so re-running is
    refused rather than replaying from dirty state."""
    g = wf.Graph()
    Doubler(g, [g.counter(period_nanos=100)])
    g.run(cycles=2)
    with pytest.raises(Exception):
        g.run(cycles=2)


def test_upstream_value_repr_is_useful():
    assert "UpstreamValue(1.5)" == repr(wf.stream.UpstreamValue(1.5))
