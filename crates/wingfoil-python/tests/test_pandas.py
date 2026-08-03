"""pandas interop — the multi-stream ``build_dataframe`` join.

The parity oracle is ``legacy/wingfoil-python/tests/test_pandas.py``. Its four
stream-level tests (``test_dict_of_streams``, ``test_async_frequencies``,
``test_massive_fan_out``, ``test_build_dataframe_skips_empty_streams``) are
ported here one for one.

Two mapping notes:

* legacy's ``stream.dataframe()`` accumulated ``(time, value)`` tuples — that is
  next's ``stream.collect()``. Next's ``stream.dataframe()`` is the *upgraded*
  half: a real ``pandas.DataFrame`` assembled in Rust. ``build_dataframe``
  accepts either shape, so both are exercised below.
* legacy timestamps were seconds as floats; next's are nanoseconds as ints (the
  established ``with_time`` convention). Tick *times* are asserted, tick
  *values* are unchanged from legacy.

The legacy file's other seven tests cover ``to_dataframe``, a pure-Python
list-to-frame converter with no next counterpart by design — next builds the
frame in the engine (``stream.dataframe()``), so there is no free function to
test. See ``docs/migration.rst``.
"""

import pytest

import wingfoil as wf

pd = pytest.importorskip("pandas")


def test_dict_of_streams():
    """Legacy ``test_dict_of_streams`` — two branches of one source, joined."""
    g = wf.Graph()
    source = g.counter(period_nanos=100)  # 1, 2, 3 at t = 0, 100, 200

    stream_a = source.map(lambda i: i - 1).dataframe()
    stream_b = source.map(lambda i: (i - 1) * 2).dataframe()

    g.run(cycles=3)

    df = wf.build_dataframe({"col_a": stream_a, "col_b": stream_b})

    assert len(df) == 3
    assert "time" in df.columns
    assert "col_a" in df.columns
    assert "col_b" in df.columns

    assert list(df["time"]) == [0, 100, 200]
    assert df.iloc[0]["col_a"] == 0
    assert df.iloc[0]["col_b"] == 0
    assert df.iloc[2]["col_a"] == 2
    assert df.iloc[2]["col_b"] == 4


def test_async_frequencies():
    """Legacy ``test_async_frequencies`` — two independent tickers at different
    speeds, outer-joined with NaNs where the slow one was silent.

    Collected with ``collect()`` rather than ``dataframe()``: ``dataframe()``
    materialises the frame on the run's *last cycle*, which a stream that has
    gone quiet by then never sees. ``collect()`` re-emits its rows on every tick
    and is what legacy's ``dataframe()` actually was.
    """
    g = wf.Graph()
    fast_stream = g.counter(period_nanos=100).map(lambda x: x * 10).collect()
    slow_stream = g.counter(period_nanos=200).map(lambda x: x * 100).collect()

    g.run(cycles=4)  # t = 0, 100, 200, 300 — slow ticks only at 0 and 200

    df = wf.build_dataframe({"fast": fast_stream, "slow": slow_stream})

    assert len(df) == 4
    assert "fast" in df.columns
    assert "slow" in df.columns

    assert pd.isna(df.iloc[1]["slow"])
    assert pd.isna(df.iloc[3]["slow"])
    # But it should have values where they align.
    assert not pd.isna(df.iloc[0]["slow"])
    assert df.iloc[0]["slow"] == 100
    assert df.iloc[2]["slow"] == 200
    assert list(df["fast"]) == [10, 20, 30, 40]


def test_massive_fan_out():
    """Legacy ``test_massive_fan_out`` — one source, three branches, all aligned."""
    g = wf.Graph()
    source = g.counter(period_nanos=100)  # 1, 2, 3

    s_add = source.map(lambda x: x + 5).dataframe()
    s_sub = source.map(lambda x: x - 5).dataframe()
    s_mult = source.map(lambda x: x * 5).dataframe()

    g.run(cycles=3)

    df = wf.build_dataframe({"add": s_add, "sub": s_sub, "mult": s_mult})

    assert len(df) == 3
    assert all(col in df.columns for col in ["time", "add", "sub", "mult"])

    assert df.iloc[2]["add"] == 8
    assert df.iloc[2]["sub"] == -2
    assert df.iloc[2]["mult"] == 15


def test_build_dataframe_skips_empty_streams():
    """Legacy ``test_build_dataframe_skips_empty_streams`` — a stream that never
    ran contributes no column.

    Legacy ran a single stream's own sub-graph; next runs a whole ``Graph``, so
    the never-run stream lives on a second graph that is simply left alone.
    """
    empty_graph = wf.Graph()
    empty_stream = empty_graph.counter(period_nanos=100).dataframe()

    live_graph = wf.Graph()
    live_stream = live_graph.counter(period_nanos=100).dataframe()
    live_graph.run(cycles=3)

    df = wf.build_dataframe({"empty": empty_stream, "live": live_stream})

    assert "live" in df.columns
    assert "empty" not in df.columns
    assert len(df) == 3


# Beyond the legacy four: contract details the legacy helper had but never
# pinned with a test, plus the shapes next adds.


def test_build_dataframe_with_no_columns_is_empty():
    """Every stream empty (or none supplied at all) yields an empty frame."""
    empty_stream = wf.Graph().counter(period_nanos=100).dataframe()

    assert wf.build_dataframe({}).empty
    assert wf.build_dataframe({"empty": empty_stream}).empty


def test_build_dataframe_preserves_column_order():
    """Columns follow the dict's insertion order, after the joined `time`."""
    g = wf.Graph()
    source = g.counter(period_nanos=100)
    third = source.map(lambda i: i * 3).dataframe()
    first = source.map(lambda i: i).dataframe()
    second = source.map(lambda i: i * 2).dataframe()
    g.run(cycles=2)

    df = wf.build_dataframe({"c": third, "a": first, "b": second})

    assert list(df.columns) == ["time", "c", "a", "b"]


def test_build_dataframe_mixes_frame_and_tuple_columns():
    """A `dataframe()` column and a `collect()` column join interchangeably."""
    g = wf.Graph()
    source = g.counter(period_nanos=100)
    framed = source.map(lambda i: i * 10).dataframe()
    collected = source.map(lambda i: i * 100).collect()
    g.run(cycles=3)

    df = wf.build_dataframe({"framed": framed, "collected": collected})

    assert list(df["time"]) == [0, 100, 200]
    assert list(df["framed"]) == [10, 20, 30]
    assert list(df["collected"]) == [100, 200, 300]


def test_build_dataframe_rejects_non_stream_values():
    """A dict value that is not a Stream is a clear error, not a crash."""
    with pytest.raises(ValueError, match="not a wingfoil Stream"):
        wf.build_dataframe({"nope": [(0, 1)]})
