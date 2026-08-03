"""Tests for the wingfoil KDB+ Python bindings.

Two groups:

- Unit-level wiring / marshaling tests (no marker): run by default, with no live
  service. They exercise the ``#[pyadapter]`` glue — argument defaults, the
  wiring-time validation a fallible adapter surfaces as an exception, and the
  dict-to-column marshaling — without touching the network.
- Integration tests (``@pytest.mark.requires_kdb``): deselected by default. The
  kdb-next integration workflow opts in with ``-m requires_kdb``; without a q
  process on localhost:5000 they fail loudly rather than skipping.

Local setup::

    q -p 5000

KDB+ has no public, freely-licensed container image, so — as with the Rust
integration tests — the instance is externally provided. ``KDB_TEST_HOST`` /
``KDB_TEST_PORT`` override the target.
"""

import os
import socket
import struct

import pytest

import wingfoil as wf

HOST = os.environ.get("KDB_TEST_HOST", "localhost")
PORT = int(os.environ.get("KDB_TEST_PORT", "5000"))

# Loopback port 1 is reserved and never hosts a service, so every unit-level
# test below wires against something that provably cannot connect.
UNREACHABLE_PORT = 1

READ_TABLE = "py_kdb_trades"
WRITE_TABLE = "py_kdb_written"
# Deliberately *not* time-first: the binding's wrapper `xcols`-es the time
# column to position 0, so the caller's select order does not matter.
QUERY = f"select sym, price, qty, time from {READ_TABLE}"
COLUMNS = [("sym", "symbol"), ("price", "float"), ("qty", "long")]

SECOND_NANOS = 1_000_000_000
DAY_NANOS = 86_400 * SECOND_NANOS
# 2000-01-01 00:00:00 UTC in nanoseconds — the KDB epoch, and the historical
# start every read below is wired for.
KDB_EPOCH_NANOS = 946_684_800 * SECOND_NANOS


# ---- Unit-level coverage (no live KDB+) ----


def test_module_exposes_the_kdb_surface():
    for name in ("kdb_read", "kdb_sub", "kdb_write"):
        assert callable(getattr(wf, name)), name


def test_read_constructs_a_stream():
    g = wf.Graph()
    stream = wf.kdb_read(
        g,
        HOST,
        UNREACHABLE_PORT,
        QUERY,
        "time",
        KDB_EPOCH_NANOS,
        DAY_NANOS,
        chunk_secs=86400,
    )
    assert isinstance(stream, wf.Stream)


def test_read_optional_args_have_defaults():
    """`chunk_secs` / `buffer_size` / credentials are optional."""
    g = wf.Graph()
    assert isinstance(
        wf.kdb_read(
            g, HOST, UNREACHABLE_PORT, QUERY, "time", KDB_EPOCH_NANOS, DAY_NANOS
        ),
        wf.Stream,
    )


def test_read_rejects_an_unbounded_window_at_wiring():
    """A fallible adapter's wiring error surfaces as a Python exception."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.kdb_read(g, HOST, UNREACHABLE_PORT, QUERY, "time", 0, DAY_NANOS)
    assert "start_time" in str(excinfo.value)


def test_read_rejects_a_zero_chunk_size_at_wiring():
    g = wf.Graph()
    with pytest.raises(RuntimeError):
        wf.kdb_read(
            g,
            HOST,
            UNREACHABLE_PORT,
            QUERY,
            "time",
            KDB_EPOCH_NANOS,
            DAY_NANOS,
            chunk_secs=0,
        )


def test_read_rejects_half_a_credential():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.kdb_read(
            g,
            HOST,
            UNREACHABLE_PORT,
            QUERY,
            "time",
            KDB_EPOCH_NANOS,
            DAY_NANOS,
            username="u",
        )
    assert "together" in str(excinfo.value)


def test_sub_constructs_a_stream():
    g = wf.Graph()
    stream = wf.kdb_sub(g, HOST, UNREACHABLE_PORT, READ_TABLE)
    assert isinstance(stream, wf.Stream)


def test_sub_rejects_historical_mode_at_wiring():
    """The tickerplant tail is live-only — a backtest has no timeline to replay."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.kdb_sub(g, HOST, UNREACHABLE_PORT, READ_TABLE, realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


def test_write_constructs_a_terminal_stream():
    g = wf.Graph()
    sink = wf.kdb_write(
        g.constant({"sym": "A", "price": 1.0, "qty": 2}),
        HOST,
        UNREACHABLE_PORT,
        WRITE_TABLE,
        COLUMNS,
    )
    assert isinstance(sink, wf.Stream)


def _run_write(value, columns=COLUMNS):
    """Wire and run a one-tick write of `value`, returning the raised error.

    Marshaling happens in the graph cycle, before the sink's lazy connect, so
    these assertions never reach the network.
    """
    g = wf.Graph()
    wf.kdb_write(g.constant(value), HOST, UNREACHABLE_PORT, WRITE_TABLE, columns)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(cycles=1, start_nanos=KDB_EPOCH_NANOS)
    return str(excinfo.value)


def test_write_missing_key_errors():
    assert "missing column 'price'" in _run_write({"sym": "A", "qty": 2})


def test_write_wrong_value_type_errors():
    assert "column 'price'" in _run_write({"sym": "A", "price": "nope", "qty": 2})


def test_write_unsupported_declared_type_errors():
    message = _run_write({"id": "x"}, columns=[("id", "guid")])
    assert "unsupported column type 'guid'" in message


def test_write_non_dict_value_errors():
    assert "must be a dict" in _run_write(42.0)


# ---- Integration tests (require KDB+ on localhost:5000) ----
#
# A q expression is sent over KDB's IPC protocol directly rather than through a
# client library: there is no maintained, freely-installable Python q client,
# and the setup here only ever needs "run this and tell me if it failed". The
# framing is small enough to spell out — see `_q` below — and every *value*
# assertion goes through the adapter under test, so nothing here decodes a K.

_HANDSHAKE = b"\x03\x00"  # anonymous credential + capability byte + null
_SYNC = 1  # message type: synchronous (the reply tells us whether q errored)
_STRING = 10  # q type indicator for a char vector — a text query
_ERROR = 0x80  # q type indicator for an error object (-128 as a signed byte)


def _recv_exactly(sock, count):
    chunks = []
    while count:
        chunk = sock.recv(count)
        if not chunk:
            raise RuntimeError("KDB+ closed the connection mid-message")
        chunks.append(chunk)
        count -= len(chunk)
    return b"".join(chunks)


def _q(expr):
    """Run one q expression synchronously, raising on a q error."""
    with socket.create_connection((HOST, PORT), timeout=10) as sock:
        sock.sendall(_HANDSHAKE)
        if not sock.recv(1):
            raise RuntimeError(f"KDB+ at {HOST}:{PORT} rejected the handshake")

        body = expr.encode()
        payload = bytes([_STRING, 0]) + struct.pack("<I", len(body)) + body
        header = bytes([1, _SYNC, 0, 0]) + struct.pack("<I", 8 + len(payload))
        sock.sendall(header + payload)

        reply_header = _recv_exactly(sock, 8)
        compressed = reply_header[2]
        length = struct.unpack("<I", reply_header[4:8])[0]
        reply = _recv_exactly(sock, length - 8)
        if compressed:
            # q only compresses replies over 2000 bytes; nothing here is close.
            raise RuntimeError(f"unexpected compressed reply to `{expr}`")
        if reply[0] == _ERROR:
            message = reply[1:].split(b"\x00", 1)[0].decode()
            raise RuntimeError(f"q error for `{expr}`: {message}")


def _reset_read_table(rows=()):
    """Create a fresh read table and seed `(offset_secs, sym, price, qty)` rows."""
    _q(
        f"{READ_TABLE}:([]time:`timestamp$();sym:`symbol$();"
        f"price:`float$();qty:`long$())"
    )
    for offset, sym, price, qty in rows:
        # A plain timestamp literal, the same form the Rust sink emits — q has
        # no cast from a `0D…` timespan to a timestamp.
        _q(
            f"insert[`{READ_TABLE}; (enlist 2000.01.01D00:00:{offset:02d};"
            f" enlist `{sym}; enlist {price}f; enlist {qty}j)]"
        )


def _reset_write_table():
    _q(
        f"{WRITE_TABLE}:([]time:`timestamp$();sym:`symbol$();"
        f"price:`float$();qty:`long$())"
    )


def _read_back(table, columns):
    """Read a whole KDB day back through `kdb_read`, returning the tick lists."""
    g = wf.Graph()
    out = wf.kdb_read(
        g,
        HOST,
        PORT,
        f"select {columns} from {table}",
        "time",
        KDB_EPOCH_NANOS,
        DAY_NANOS,
        chunk_secs=86400,
    ).accumulate()
    g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)
    return out.value()


@pytest.mark.requires_kdb
def test_read_returns_lists_of_row_dicts():
    _reset_read_table([(1, "AAPL", 100.0, 10), (2, "GOOG", 200.0, 20)])

    ticks = _read_back(READ_TABLE, "time, sym, price, qty")

    # Each tick is a *list* of the rows sharing that timestamp — lossless,
    # unlike legacy's collapse-to-one-dict.
    assert [[row["sym"] for row in tick] for tick in ticks] == [["AAPL"], ["GOOG"]]
    assert ticks[0][0]["qty"] == 10
    assert ticks[1][0]["price"] == 200.0


@pytest.mark.requires_kdb
def test_read_groups_rows_sharing_a_timestamp():
    """Two rows at one instant arrive on one tick, both of them."""
    _reset_read_table([(1, "AAPL", 100.0, 10), (1, "GOOG", 200.0, 20)])

    ticks = _read_back(READ_TABLE, "time, sym, price, qty")

    assert [[row["sym"] for row in tick] for tick in ticks] == [["AAPL", "GOOG"]]


@pytest.mark.requires_kdb
def test_read_xcols_the_time_column_whatever_the_select_order():
    """The wrapper reorders, so a query that selects `time` last still decodes."""
    _reset_read_table([(1, "AAPL", 100.0, 10)])

    g = wf.Graph()
    out = wf.kdb_read(
        g, HOST, PORT, QUERY, "time", KDB_EPOCH_NANOS, DAY_NANOS, chunk_secs=86400
    ).collect()
    g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)

    times = [nanos for nanos, _ in out.value()]
    assert times == [KDB_EPOCH_NANOS + SECOND_NANOS]
    # The time column is consumed as the tick time, not repeated in the dict.
    assert sorted(out.value()[0][1][0]) == ["price", "qty", "sym"]


@pytest.mark.requires_kdb
def test_write_round_trip():
    _reset_write_table()

    g = wf.Graph()
    wf.kdb_write(
        g.constant({"sym": "TEST", "price": 42.0, "qty": 100}),
        HOST,
        PORT,
        WRITE_TABLE,
        COLUMNS,
    )
    g.run(cycles=1, start_nanos=KDB_EPOCH_NANOS)

    ticks = _read_back(WRITE_TABLE, "time, sym, price, qty")
    assert len(ticks) == 1 and len(ticks[0]) == 1
    row = ticks[0][0]
    assert row["sym"] == "TEST"
    assert row["price"] == pytest.approx(42.0)
    assert row["qty"] == 100


@pytest.mark.requires_kdb
def test_write_a_list_of_dicts_inserts_every_row():
    """A Python list on one tick is a multi-row burst — all of it is written."""
    _reset_write_table()

    g = wf.Graph()
    wf.kdb_write(
        g.constant(
            [
                {"sym": "AAA", "price": 1.0, "qty": 1},
                {"sym": "BBB", "price": 2.0, "qty": 2},
            ]
        ),
        HOST,
        PORT,
        WRITE_TABLE,
        COLUMNS,
    )
    g.run(cycles=1, start_nanos=KDB_EPOCH_NANOS)

    ticks = _read_back(WRITE_TABLE, "time, sym, qty")
    assert [(row["sym"], row["qty"]) for tick in ticks for row in tick] == [
        ("AAA", 1),
        ("BBB", 2),
    ]


@pytest.mark.requires_kdb
def test_read_unsupported_column_type_aborts_the_run():
    """An undecodable column aborts on the first row rather than yielding a value.

    Which *message* comes back depends on where the type is rejected: a `byte`
    column is refused by the row accessor before the binding's own dispatch
    sees it, so only the abort is asserted here. The dispatch's own message is
    covered by the Rust `an_unsupported_column_type_errors_rather_than_debug_rendering`
    test, which can construct the value directly.
    """
    table = "py_kdb_bytes"
    _q(f"{table}:([]time:`timestamp$();raw:`byte$())")
    _q(f"insert[`{table}; (enlist 2000.01.01D00:00:01; enlist 0x2a)]")

    g = wf.Graph()
    wf.kdb_read(
        g,
        HOST,
        PORT,
        f"select time, raw from {table}",
        "time",
        KDB_EPOCH_NANOS,
        DAY_NANOS,
        chunk_secs=86400,
    )
    with pytest.raises(RuntimeError):
        g.run(start_nanos=KDB_EPOCH_NANOS, duration_nanos=DAY_NANOS)
