"""Tests for the wingfoil polars Python bindings.

polars needs nothing running — so there is no ``requires_*`` marker here and
the whole file runs by default. The round trips write a file with
``polars_write`` and replay it with ``polars_read``, so the tests need no
Python ``polars`` package either.
"""

import pytest

import wingfoil as wf

SECOND_NANOS = 1_000_000_000


def write_rows(path, rows, **kwargs):
    """Write ``rows`` (one tick per second from t=0) to ``path``."""
    g = wf.Graph()
    stream = g.values(rows, period_nanos=SECOND_NANOS)
    wf.polars_write(stream, str(path), **kwargs)
    g.run(realtime=False, start_nanos=0, duration_nanos=len(rows) * SECOND_NANOS)


def replay(path, **kwargs):
    g = wf.Graph()
    seen = []
    wf.polars_read(g, str(path), "time", **kwargs).with_time().inspect(seen.append)
    g.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)
    return seen


def test_module_exposes_the_polars_surface():
    for name in ("polars_read", "polars_write"):
        assert callable(getattr(wf, name)), name


@pytest.mark.parametrize("name", ["quotes.parquet", "quotes.arrow"])
def test_a_written_file_replays_at_its_graph_times(tmp_path, name):
    path = tmp_path / name
    write_rows(
        path,
        [
            {"sym": "AAPL", "px": 1.5, "qty": 10, "live": True, "note": None},
            {"sym": "MSFT", "px": 2.5, "qty": 20, "live": False, "note": "x"},
        ],
    )

    assert [
        (0, [{"sym": "AAPL", "px": 1.5, "qty": 10, "live": True, "note": None}]),
        (
            SECOND_NANOS,
            [{"sym": "MSFT", "px": 2.5, "qty": 20, "live": False, "note": "x"}],
        ),
    ] == replay(path)


def test_rows_sharing_an_instant_arrive_in_one_tick(tmp_path):
    path = tmp_path / "burst.parquet"
    write_rows(path, [[{"sym": "AAPL"}, {"sym": "MSFT"}, {"sym": "GOOG"}]])

    seen = replay(path)
    assert 1 == len(seen), "one tick"
    assert ["AAPL", "MSFT", "GOOG"] == [row["sym"] for row in seen[0][1]]


def test_bytes_round_trip(tmp_path):
    path = tmp_path / "bytes.arrow"
    write_rows(path, [{"raw": b"\x00\x01"}])
    assert [(0, [{"raw": b"\x00\x01"}])] == replay(path)


def test_the_time_column_is_the_tick_time_not_a_dict_key(tmp_path):
    path = tmp_path / "keys.parquet"
    write_rows(path, [{"zeta": 1, "alpha": 2}])
    (_, rows) = replay(path)[0]
    assert ["zeta", "alpha"] == list(rows[0].keys()), "column order, no 'time'"


def test_an_explicit_format_overrides_the_extension(tmp_path):
    path = tmp_path / "quotes.bin"
    write_rows(path, [{"px": 1.0}], format="ipc")
    assert [(0, [{"px": 1.0}])] == replay(path, format="ipc")


def test_read_buffer_size_is_optional(tmp_path):
    path = tmp_path / "buffered.parquet"
    write_rows(path, [{"px": 1.0}])
    assert [(0, [{"px": 1.0}])] == replay(path, buffer_size=1)


def test_read_rejects_a_missing_file_at_wiring(tmp_path):
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.polars_read(g, str(tmp_path / "nope.parquet"), "time")
    assert "nope.parquet" in str(excinfo.value)


def test_read_rejects_an_unknown_time_column_at_wiring(tmp_path):
    """The error names the file's actual columns."""
    path = tmp_path / "cols.parquet"
    write_rows(path, [{"sym": "AAPL"}])
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.polars_read(g, str(path), "timestamp")
    message = str(excinfo.value)
    assert "no time column 'timestamp'" in message
    assert "sym" in message


def test_read_rejects_an_unknown_extension_or_format(tmp_path):
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.polars_read(g, str(tmp_path / "quotes.csv"), "time")
    assert ".parquet" in str(excinfo.value)
    with pytest.raises(RuntimeError) as excinfo:
        wf.polars_read(g, str(tmp_path / "quotes.parquet"), "time", format="csv")
    assert '"parquet"' in str(excinfo.value)


def test_write_can_omit_the_time_column(tmp_path):
    path = tmp_path / "no_time.parquet"
    write_rows(path, [{"px": 1.0}], time_column=None)
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.polars_read(g, str(path), "time")
    assert "no time column 'time'" in str(excinfo.value)


def test_write_rejects_an_unwritable_path_at_wiring(tmp_path):
    g = wf.Graph()
    stream = g.values([{"px": 1.0}], period_nanos=SECOND_NANOS)
    with pytest.raises(RuntimeError) as excinfo:
        wf.polars_write(stream, str(tmp_path / "missing" / "out.parquet"))
    assert "missing" in str(excinfo.value)


def test_write_aborts_on_a_column_changing_type(tmp_path):
    g = wf.Graph()
    stream = g.values([{"px": 1}, {"px": 1.5}], period_nanos=SECOND_NANOS)
    wf.polars_write(stream, str(tmp_path / "mixed.parquet"))
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)
    assert "column 'px'" in str(excinfo.value)


def test_write_aborts_on_an_unsupported_value(tmp_path):
    g = wf.Graph()
    stream = g.values([{"xs": [1, 2]}], period_nanos=SECOND_NANOS)
    wf.polars_write(stream, str(tmp_path / "list.parquet"))
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)
    assert "column 'xs'" in str(excinfo.value)
