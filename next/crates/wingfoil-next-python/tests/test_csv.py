"""Tests for the wingfoil-next CSV Python bindings.

Unlike the service-backed adapters, CSV needs nothing running — so there is no
``requires_*`` marker here and the whole file runs by default, round trips
included.
"""

import pytest

import wingfoil_next as wf

SECOND_NANOS = 1_000_000_000


def write_csv(tmp_path, name, text):
    path = tmp_path / name
    path.write_text(text)
    return str(path)


# ---- Reading ----


def test_module_exposes_the_csv_surface():
    for name in ("csv_read", "csv_write"):
        assert callable(getattr(wf, name)), name


def test_read_yields_a_list_of_dicts_per_tick(tmp_path):
    path = write_csv(
        tmp_path,
        "trades.csv",
        "time,sym,px\n0,AAPL,1.5\n1000000000,MSFT,2.5\n",
    )
    g = wf.Graph()
    seen = []
    wf.csv_read(g, path, "time").inspect(seen.append)
    g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)

    assert [
        [{"time": "0", "sym": "AAPL", "px": "1.5"}],
        [{"time": "1000000000", "sym": "MSFT", "px": "2.5"}],
    ] == seen


def test_rows_sharing_a_timestamp_arrive_in_one_tick(tmp_path):
    """The lossless-burst deviation: legacy kept only the last of these."""
    path = write_csv(
        tmp_path,
        "same_time.csv",
        "time,sym\n0,AAPL\n0,MSFT\n0,GOOG\n",
    )
    g = wf.Graph()
    seen = []
    wf.csv_read(g, path, "time").inspect(seen.append)
    g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)

    assert 1 == len(seen), "one tick"
    assert ["AAPL", "MSFT", "GOOG"] == [row["sym"] for row in seen[0]]


def test_dict_keys_follow_the_file_column_order(tmp_path):
    """Legacy decoded into a HashMap, so key order was arbitrary."""
    path = write_csv(tmp_path, "order.csv", "time,zeta,alpha,mid\n0,z,a,m\n")
    g = wf.Graph()
    seen = []
    wf.csv_read(g, path, "time").inspect(seen.append)
    g.run(realtime=False, start_nanos=0, duration_nanos=SECOND_NANOS)

    assert ["time", "zeta", "alpha", "mid"] == list(seen[0][0].keys())


def test_the_time_column_need_not_be_first(tmp_path):
    path = write_csv(tmp_path, "late_time.csv", "sym,time\nAAPL,0\nMSFT,1000000000\n")
    g = wf.Graph()
    seen = []
    wf.csv_read(g, path, "time").inspect(seen.append)
    g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)

    assert ["AAPL", "MSFT"] == [tick[0]["sym"] for tick in seen]


def test_read_rejects_a_missing_file_at_wiring(tmp_path):
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.csv_read(g, str(tmp_path / "nope.csv"), "time")
    assert "nope.csv" in str(excinfo.value)


def test_read_rejects_an_unknown_time_column_at_wiring(tmp_path):
    """The error names the file's actual columns."""
    path = write_csv(tmp_path, "cols.csv", "time,sym\n0,AAPL\n")
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.csv_read(g, path, "timestamp")
    message = str(excinfo.value)
    assert "no column 'timestamp'" in message
    assert "sym" in message


def test_read_aborts_on_an_unparseable_timestamp(tmp_path):
    """Legacy silently substituted zero, quietly reordering the replay."""
    path = write_csv(tmp_path, "bad_time.csv", "time,sym\n0,AAPL\nnot-a-time,MSFT\n")
    g = wf.Graph()
    wf.csv_read(g, path, "time").inspect(lambda _: None)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)
    assert "unparseable" in str(excinfo.value)


def test_read_buffer_size_is_optional(tmp_path):
    path = write_csv(tmp_path, "buffered.csv", "time,sym\n0,AAPL\n")
    g = wf.Graph()
    assert isinstance(wf.csv_read(g, path, "time", buffer_size=1), wf.Stream)


# ---- Writing ----


def test_write_emits_a_header_and_rows(tmp_path):
    """The header is the reason `csv_write_with_header` was added to the engine."""
    out = str(tmp_path / "out.csv")
    g = wf.Graph()
    rows = g.values(
        [{"sym": "AAPL", "px": 1.5}, {"sym": "MSFT", "px": 2.5}],
        period_nanos=SECOND_NANOS,
    )
    wf.csv_write(rows, out, ["sym", "px"])
    g.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)

    lines = (tmp_path / "out.csv").read_text().replace("\r\n", "\n").strip().split("\n")
    assert "time,sym,px" == lines[0]
    assert 3 == len(lines)
    assert lines[1].endswith("AAPL,1.5")
    assert lines[2].endswith("MSFT,2.5")


def test_write_follows_the_declared_column_order(tmp_path):
    out = str(tmp_path / "ordered.csv")
    g = wf.Graph()
    # Dict order is deliberately the reverse of the declared columns.
    rows = g.values([{"px": 1.5, "sym": "AAPL"}], period_nanos=SECOND_NANOS)
    wf.csv_write(rows, out, ["sym", "px"])
    g.run(realtime=False, start_nanos=0, duration_nanos=SECOND_NANOS)

    lines = (tmp_path / "ordered.csv").read_text().replace("\r\n", "\n").strip().split("\n")
    assert "time,sym,px" == lines[0]
    assert lines[1].endswith("AAPL,1.5")


def test_write_with_no_columns_writes_no_header(tmp_path):
    out = str(tmp_path / "headerless.csv")
    g = wf.Graph()
    rows = g.values([{"sym": "AAPL"}], period_nanos=SECOND_NANOS)
    wf.csv_write(rows, out, [])
    g.run(realtime=False, start_nanos=0, duration_nanos=SECOND_NANOS)

    text = (tmp_path / "headerless.csv").read_text()
    assert not text.startswith("time,sym")


def test_write_aborts_on_a_missing_column(tmp_path):
    out = str(tmp_path / "missing.csv")
    g = wf.Graph()
    rows = g.values([{"sym": "AAPL"}], period_nanos=SECOND_NANOS)
    wf.csv_write(rows, out, ["sym", "px"])
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=SECOND_NANOS)
    assert "missing column 'px'" in str(excinfo.value)


def test_write_aborts_on_a_non_dict_value(tmp_path):
    out = str(tmp_path / "nondict.csv")
    g = wf.Graph()
    wf.csv_write(g.counter(period_nanos=SECOND_NANOS), out, ["sym"])
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=SECOND_NANOS)
    assert "must be a dict" in str(excinfo.value)


# ---- Round trip ----


def test_write_then_read_round_trips(tmp_path):
    """The written file is readable by `csv_read`, header and all."""
    out = str(tmp_path / "round.csv")

    write = wf.Graph()
    rows = write.values(
        [{"sym": "AAPL", "px": 1.5}, {"sym": "MSFT", "px": 2.5}],
        period_nanos=SECOND_NANOS,
    )
    wf.csv_write(rows, out, ["sym", "px"])
    write.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)

    read = wf.Graph()
    seen = []
    wf.csv_read(read, out, "time").inspect(seen.append)
    read.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)

    assert [
        [{"time": "0", "sym": "AAPL", "px": "1.5"}],
        [{"time": "1000000000", "sym": "MSFT", "px": "2.5"}],
    ] == seen
