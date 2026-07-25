"""Python round-trip tests for the wingfoil_next interop module.

These exercise the real extension module (built by maturin), proving the erased
object form is reachable and composable from Python: native values in, native
values out, Python callables wired into the graph, and run errors surfaced as
exceptions.
"""

import pytest

import wingfoil_next as wf


def test_constant_maps_via_python_lambda():
    g = wf.Graph()
    out = g.constant(4.0).map(lambda x: x * x)
    g.run(cycles=1)
    assert out.value() == 16.0


def test_counter_source_ticks():
    g = wf.Graph()
    doubled = g.counter(period_nanos=100).map(lambda n: n * 2)
    g.run(cycles=3)
    assert doubled.value() == 6  # 3rd tick -> 3 * 2


def test_filter_by_python_predicate():
    g = wf.Graph()
    counter = g.counter(period_nanos=100)
    keep = counter.map(lambda n: n > 2)
    filtered = counter.filter(keep)
    g.run(cycles=5)
    # last passing value is the 5th tick
    assert filtered.value() == 5


def test_distinct_suppresses_duplicates():
    g = wf.Graph()
    # 1,2,3,4 -> n//2 -> 0,1,1,2 -> distinct passes changes only; final value 2
    stepped = g.counter(period_nanos=100).map(lambda n: n // 2).distinct()
    g.run(cycles=4)
    assert stepped.value() == 2


def test_map_callable_exception_aborts_run():
    g = wf.Graph()
    g.constant("not a number").map(lambda x: x + 1)
    with pytest.raises(RuntimeError, match="Python map callable raised"):
        g.run(cycles=1)


def test_second_run_raises():
    g = wf.Graph()
    g.constant(1.0)
    g.run(cycles=1)
    with pytest.raises(RuntimeError, match="more than once"):
        g.run(cycles=1)


def test_native_types_round_trip():
    g = wf.Graph()
    out = g.constant("hello").map(lambda s: s.upper())
    g.run(cycles=1)
    assert out.value() == "HELLO"
