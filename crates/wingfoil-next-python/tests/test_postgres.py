"""Tests for the wingfoil-next PostgreSQL Python bindings.

Two groups:

- Unit-level wiring / marshaling tests (no marker): run by default, with no live
  service. They exercise the ``#[pyadapter]`` glue — argument defaults, the
  wiring-time validation a fallible adapter surfaces as an exception, and the
  dict-to-parameter marshaling — without touching the network.
- Integration tests (``@pytest.mark.requires_postgres``): deselected by default.
  The postgres-next integration workflow opts in with ``-m requires_postgres``;
  without PostgreSQL on localhost:5432 they fail loudly rather than skipping.

Local setup:
    docker run --rm -p 5432:5432 -e POSTGRES_PASSWORD=postgres postgres:16-alpine
"""

import pytest

import wingfoil_next as wf

CONN_STR = "host=localhost port=5432 user=postgres password=postgres dbname=postgres"
# Loopback port 1 is reserved and never hosts a service: connect is rejected.
UNREACHABLE = "host=127.0.0.1 port=1 user=postgres dbname=postgres connect_timeout=1"

QUERY = "SELECT time, sym, price, qty FROM py_pg_trades"
TABLE = "py_pg_trades"
COLUMNS = [("sym", "text"), ("price", "float"), ("qty", "long")]

SECOND_NANOS = 1_000_000_000
DAY_NANOS = 86_400 * SECOND_NANOS
# 2000-01-01 00:00:00 UTC in nanoseconds — the historical start used for reads.
KDB_EPOCH_NANOS = 946_684_800 * SECOND_NANOS


# ---- Unit-level coverage (no live PostgreSQL) ----


def test_module_exposes_the_postgres_surface():
    for name in (
        "postgres_read",
        "postgres_sub",
        "postgres_source",
        "postgres_write",
        "postgres_notify_trigger_sql",
    ):
        assert callable(getattr(wf, name)), name


def test_read_constructs_a_stream():
    g = wf.Graph()
    stream = wf.postgres_read(
        g, UNREACHABLE, QUERY, "time", KDB_EPOCH_NANOS, DAY_NANOS, chunk_secs=86400
    )
    assert isinstance(stream, wf.Stream)


def test_read_optional_args_have_defaults():
    """`chunk_secs` / `buffer_size` are optional — the forwarded pyo3 signature."""
    g = wf.Graph()
    assert isinstance(
        wf.postgres_read(g, UNREACHABLE, QUERY, "time", KDB_EPOCH_NANOS, DAY_NANOS),
        wf.Stream,
    )


def test_read_rejects_an_unbounded_window_at_wiring():
    """A fallible adapter's wiring error surfaces as a Python exception."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.postgres_read(g, UNREACHABLE, QUERY, "time", 0, DAY_NANOS)
    assert "start_time" in str(excinfo.value)


def test_read_rejects_a_zero_chunk_size_at_wiring():
    g = wf.Graph()
    with pytest.raises(RuntimeError):
        wf.postgres_read(
            g, UNREACHABLE, QUERY, "time", KDB_EPOCH_NANOS, DAY_NANOS, chunk_secs=0
        )


def test_sub_constructs_a_stream():
    g = wf.Graph()
    stream = wf.postgres_sub(g, UNREACHABLE, QUERY, "time", "py_pg_feed")
    assert isinstance(stream, wf.Stream)


def test_sub_rejects_historical_mode_at_wiring():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.postgres_sub(g, UNREACHABLE, QUERY, "time", "py_pg_feed", realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


def test_source_dispatches_on_the_declared_run_mode():
    """One wiring call, either mode — next's unified `<adapter>_source`."""
    g = wf.Graph()
    historical = wf.postgres_source(
        g,
        UNREACHABLE,
        QUERY,
        "time",
        "py_pg_feed",
        False,
        start_nanos=KDB_EPOCH_NANOS,
        duration_nanos=DAY_NANOS,
    )
    live = wf.postgres_source(g, UNREACHABLE, QUERY, "time", "py_pg_feed", True)
    assert isinstance(historical, wf.Stream)
    assert isinstance(live, wf.Stream)


def test_source_historical_still_validates_its_window():
    g = wf.Graph()
    with pytest.raises(RuntimeError):
        wf.postgres_source(g, UNREACHABLE, QUERY, "time", "py_pg_feed", False)


def test_write_constructs_a_terminal_stream():
    g = wf.Graph()
    sink = wf.postgres_write(
        g.constant({"sym": "A", "price": 1.0, "qty": 2}), UNREACHABLE, TABLE, COLUMNS
    )
    assert isinstance(sink, wf.Stream)


def _run_write(value, columns=COLUMNS):
    """Wire and run a one-tick write of `value`, returning the raised error."""
    g = wf.Graph()
    wf.postgres_write(g.constant(value), UNREACHABLE, TABLE, columns)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(cycles=1, start_nanos=KDB_EPOCH_NANOS)
    return str(excinfo.value)


def test_write_missing_key_errors():
    assert "missing column 'price'" in _run_write({"sym": "A", "qty": 2})


def test_write_wrong_value_type_errors():
    assert "column 'price'" in _run_write({"sym": "A", "price": "nope", "qty": 2})


def test_write_unsupported_declared_type_errors():
    message = _run_write({"id": "x"}, columns=[("id", "uuid")])
    assert "unsupported column type 'uuid'" in message


def test_write_non_dict_value_errors():
    assert "must be a dict" in _run_write(42.0)


def test_notify_trigger_sql_shape():
    sql = wf.postgres_notify_trigger_sql(TABLE, "py_pg_feed")
    assert "pg_notify('py_pg_feed', '')" in sql
    assert f'AFTER INSERT ON "{TABLE}"' in sql


# ---- Integration tests (require PostgreSQL on localhost:5432) ----


def _reset_table(sql_rows=()):
    """Create a fresh `py_pg_trades` table and optionally seed rows (needs psycopg)."""
    import psycopg

    with psycopg.connect(CONN_STR, autocommit=True) as conn:
        conn.execute(f"DROP TABLE IF EXISTS {TABLE}")
        conn.execute(
            f"CREATE TABLE {TABLE} (time timestamp, sym text, price float8, qty int8)"
        )
        for ts, sym, price, qty in sql_rows:
            conn.execute(
                f"INSERT INTO {TABLE} VALUES (%s, %s, %s, %s)", (ts, sym, price, qty)
            )


@pytest.mark.requires_postgres
def test_read_returns_lists_of_row_dicts():
    _reset_table(
        [
            ("2000-01-01 00:00:01", "AAPL", 100.0, 10),
            ("2000-01-01 00:00:02", "GOOG", 200.0, 20),
            ("2000-01-01 00:00:03", "MSFT", 300.0, 30),
        ]
    )

    g = wf.Graph()
    out = wf.postgres_read(
        g, CONN_STR, QUERY, "time", KDB_EPOCH_NANOS, DAY_NANOS, chunk_secs=86400
    ).accumulate()
    g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)

    ticks = out.value()
    # Each tick is a *list* of the rows sharing that timestamp — lossless, unlike
    # legacy's collapse-to-one-dict.
    assert [[row["sym"] for row in tick] for tick in ticks] == [
        ["AAPL"],
        ["GOOG"],
        ["MSFT"],
    ]
    assert ticks[0][0]["qty"] == 10
    assert ticks[1][0]["price"] == 200.0


@pytest.mark.requires_postgres
def test_read_ticks_at_the_row_timestamps():
    """The row's time column becomes the on-graph tick time."""
    _reset_table(
        [
            ("2000-01-01 00:00:01", "AAPL", 100.0, 10),
            ("2000-01-01 00:00:02", "GOOG", 200.0, 20),
        ]
    )

    g = wf.Graph()
    out = wf.postgres_read(
        g, CONN_STR, QUERY, "time", KDB_EPOCH_NANOS, DAY_NANOS, chunk_secs=86400
    ).collect()
    g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)

    times = [nanos for nanos, _ in out.value()]
    assert times == [KDB_EPOCH_NANOS + SECOND_NANOS, KDB_EPOCH_NANOS + 2 * SECOND_NANOS]


@pytest.mark.requires_postgres
def test_write_round_trip():
    import psycopg

    _reset_table()

    g = wf.Graph()
    # The table's qty column is int8, so the declared type is "long".
    wf.postgres_write(
        g.constant({"sym": "TEST", "price": 42.0, "qty": 100}),
        CONN_STR,
        TABLE,
        COLUMNS,
    )
    g.run(cycles=1, start_nanos=KDB_EPOCH_NANOS)

    with psycopg.connect(CONN_STR) as conn:
        row = conn.execute(f"SELECT sym, price, qty FROM {TABLE}").fetchone()
    assert row[0] == "TEST"
    assert row[1] == pytest.approx(42.0)
    assert row[2] == 100


@pytest.mark.requires_postgres
def test_write_a_list_of_dicts_inserts_every_row():
    """A Python list on one tick is a multi-row burst — all of it is written."""
    import psycopg

    _reset_table()

    g = wf.Graph()
    wf.postgres_write(
        g.constant(
            [
                {"sym": "AAA", "price": 1.0, "qty": 1},
                {"sym": "BBB", "price": 2.0, "qty": 2},
            ]
        ),
        CONN_STR,
        TABLE,
        COLUMNS,
    )
    g.run(cycles=1, start_nanos=KDB_EPOCH_NANOS)

    with psycopg.connect(CONN_STR) as conn:
        rows = conn.execute(f"SELECT sym, qty FROM {TABLE} ORDER BY sym").fetchall()
    assert rows == [("AAA", 1), ("BBB", 2)]


@pytest.mark.requires_postgres
def test_write_int4_and_null_round_trip():
    """`int` binds as int4 (plain `integer` columns work); None writes SQL NULL."""
    import psycopg

    with psycopg.connect(CONN_STR, autocommit=True) as conn:
        conn.execute("DROP TABLE IF EXISTS py_pg_int4")
        conn.execute("CREATE TABLE py_pg_int4 (time timestamp, qty integer, note text)")

    g = wf.Graph()
    wf.postgres_write(
        g.constant({"qty": 7, "note": None}),
        CONN_STR,
        "py_pg_int4",
        [("qty", "int"), ("note", "text")],
    )
    g.run(cycles=1, start_nanos=KDB_EPOCH_NANOS)

    with psycopg.connect(CONN_STR) as conn:
        row = conn.execute("SELECT qty, note FROM py_pg_int4").fetchone()
    assert row[0] == 7
    assert row[1] is None

    # Read back through the adapter: int4 dispatch + SQL NULL -> Python None.
    g = wf.Graph()
    out = wf.postgres_read(
        g,
        CONN_STR,
        "SELECT time, qty, note FROM py_pg_int4",
        "time",
        KDB_EPOCH_NANOS,
        DAY_NANOS,
        chunk_secs=86400,
    ).accumulate()
    g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)

    ticks = out.value()
    assert len(ticks) == 1
    assert ticks[0][0]["qty"] == 7
    assert ticks[0][0]["note"] is None


@pytest.mark.requires_postgres
def test_read_unsupported_column_type_aborts_the_run():
    """A `numeric` column fails on the first row rather than reading as None."""
    import psycopg

    with psycopg.connect(CONN_STR, autocommit=True) as conn:
        conn.execute("DROP TABLE IF EXISTS py_pg_numeric")
        conn.execute("CREATE TABLE py_pg_numeric (time timestamp, amount numeric)")
        conn.execute(
            "INSERT INTO py_pg_numeric VALUES ('2000-01-01 00:00:01', 1.25)",
        )

    g = wf.Graph()
    wf.postgres_read(
        g,
        CONN_STR,
        "SELECT time, amount FROM py_pg_numeric",
        "time",
        KDB_EPOCH_NANOS,
        DAY_NANOS,
        chunk_secs=86400,
    )
    with pytest.raises(RuntimeError) as excinfo:
        g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)
    assert "unsupported column type" in str(excinfo.value)


@pytest.mark.requires_postgres
def test_sub_catch_up_then_live_inserts():
    """Seeded rows arrive via catch-up; mid-run inserts arrive via NOTIFY."""
    import gc
    import threading
    import time as time_mod

    import psycopg

    # `Graph` is an unsendable pyclass, so dropping one on a foreign thread
    # raises. `run` below releases the GIL, which lets the inserter thread
    # trigger a cyclic GC — and collect any *earlier* test's unreferenced Graph
    # there. Sweep them now so the collection happens on this thread.
    gc.collect()

    _reset_table([("2000-01-01 00:00:00", "SEED0", 1.0, 10)])
    with psycopg.connect(CONN_STR, autocommit=True) as conn:
        conn.execute(wf.postgres_notify_trigger_sql(TABLE, "py_pg_feed"))

    def insert_later():
        time_mod.sleep(0.5)
        with psycopg.connect(CONN_STR, autocommit=True) as conn:
            conn.execute(
                f"INSERT INTO {TABLE} VALUES ('2000-01-01 00:00:01', 'LIVE1', 2.0, 20)"
            )
            conn.execute(
                f"INSERT INTO {TABLE} VALUES ('2000-01-01 00:00:02', 'LIVE2', 3.0, 30)"
            )

    inserter = threading.Thread(target=insert_later)
    inserter.start()
    try:
        g = wf.Graph()
        # cursor in the past -> the catch-up query picks up SEED0
        out = wf.postgres_sub(g, CONN_STR, QUERY, "time", "py_pg_feed").accumulate()
        g.run(realtime=True, duration_nanos=3 * SECOND_NANOS)
    finally:
        inserter.join()

    # Each tick is a list of row dicts (several rows can share one real-time
    # cycle); flatten before asserting.
    syms = [row["sym"] for tick in out.value() for row in tick]
    assert syms == ["SEED0", "LIVE1", "LIVE2"]


@pytest.mark.requires_postgres
def test_source_reads_historically_with_the_same_wiring():
    """`postgres_source(realtime=False)` is the read path, wiring unchanged."""
    _reset_table([("2000-01-01 00:00:01", "AAPL", 100.0, 10)])

    g = wf.Graph()
    out = wf.postgres_source(
        g,
        CONN_STR,
        QUERY,
        "time",
        "py_pg_feed",
        False,
        start_nanos=KDB_EPOCH_NANOS,
        duration_nanos=DAY_NANOS,
        chunk_secs=86400,
    ).accumulate()
    g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)

    assert [row["sym"] for tick in out.value() for row in tick] == ["AAPL"]
