"""Tests for the wingfoil FIX Python bindings.

Two groups:

- Unit-level wiring / marshaling tests (no marker): run by default. FIX sessions
  connect at graph *start*, not at wiring, so every entry point can be wired
  against a dead port — and `FixConnection.send` marshals synchronously, which
  makes the whole dict → message path testable without a socket.
- Loopback tests (``@pytest.mark.requires_fix``): deselected by default. They
  stand up an acceptor and an initiator in **one graph** over loopback TCP, the
  same shape as `tests/fix_integration.rs`. No external service — they are
  gated only because they run against a live wall clock and are timing
  sensitive, exactly as the Rust ones are gated behind `fix-integration-test`.
  The fix-next integration workflow opts in with ``-m requires_fix``.
"""

import socket

import pytest

import wingfoil as wf

MS_NANOS = 1_000_000
SECOND_NANOS = 1_000_000_000

# Loopback port 1 is reserved and never hosts a service.
DEAD_PORT = 1

MESSAGE = {"msg_type": "V", "fields": [(262, "req1"), (263, "1")]}


def free_port():
    """An ephemeral port, released immediately — mirrors the Rust harness."""
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


# ---- Unit-level coverage (no session) ----


def test_module_exposes_the_fix_surface():
    for name in ("fix_connect", "fix_accept", "fix_send", "fix_connect_tls"):
        assert callable(getattr(wf, name)), name
    assert isinstance(wf.FixConnection, type)


def test_connect_returns_a_data_and_status_pair():
    g = wf.Graph()
    data, status = wf.fix_connect(g, "127.0.0.1", DEAD_PORT, "CLIENT", "SERVER")
    assert isinstance(data, wf.Stream)
    assert isinstance(status, wf.Stream)


def test_accept_returns_a_data_and_status_pair():
    g = wf.Graph()
    data, status = wf.fix_accept(g, free_port(), "SERVER", "CLIENT")
    assert isinstance(data, wf.Stream)
    assert isinstance(status, wf.Stream)


@pytest.mark.parametrize("mode", ["threaded", "spin"])
def test_both_poll_modes_wire(mode):
    g = wf.Graph()
    data, _ = wf.fix_connect(
        g, "127.0.0.1", DEAD_PORT, "CLIENT", "SERVER", poll_mode=mode
    )
    assert isinstance(data, wf.Stream)


def test_an_unknown_poll_mode_raises_at_wiring():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.fix_connect(
            g, "127.0.0.1", DEAD_PORT, "CLIENT", "SERVER", poll_mode="polling"
        )
    assert "expected 'threaded' or 'spin'" in str(excinfo.value)


@pytest.mark.parametrize(
    "wire",
    [
        lambda g: wf.fix_connect(
            g, "127.0.0.1", DEAD_PORT, "CLIENT", "SERVER", realtime=False
        ),
        lambda g: wf.fix_accept(g, free_port(), "SERVER", "CLIENT", realtime=False),
        lambda g: wf.fix_connect_tls(
            g, "127.0.0.1", DEAD_PORT, "USER", "VENUE", realtime=False
        ),
    ],
    ids=["connect", "accept", "connect_tls"],
)
def test_historical_mode_is_rejected_at_wiring(wire):
    """Every FIX session is live — a backtest has no timeline to replay."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wire(g)
    assert "HistoricalFrom" in str(excinfo.value)


def test_send_constructs_a_terminal_stream():
    g = wf.Graph()
    sink = wf.fix_send(g.constant(MESSAGE), "127.0.0.1", DEAD_PORT, "CLIENT", "SERVER")
    assert isinstance(sink, wf.Stream)


def test_connect_tls_returns_a_connection_handle():
    g = wf.Graph()
    conn = wf.fix_connect_tls(g, "127.0.0.1", DEAD_PORT, "USER", "VENUE")
    assert isinstance(conn, wf.FixConnection)
    assert isinstance(conn.data, wf.Stream)
    assert isinstance(conn.status, wf.Stream)


def test_fix_sub_constructs_a_terminal_stream():
    g = wf.Graph()
    conn = wf.fix_connect_tls(g, "127.0.0.1", DEAD_PORT, "USER", "VENUE")
    assert isinstance(conn.fix_sub(g.constant(["4001", "4002"])), wf.Stream)


# `FixConnection.send` marshals the dict synchronously, before the message ever
# reaches the session thread — so it is the marshaling path's unit harness.


def _send(msg):
    g = wf.Graph()
    conn = wf.fix_connect_tls(g, "127.0.0.1", DEAD_PORT, "USER", "VENUE")
    conn.send(msg)


def _send_error(msg):
    with pytest.raises(RuntimeError) as excinfo:
        _send(msg)
    return str(excinfo.value)


def test_a_well_formed_message_marshals():
    _send(MESSAGE)


def test_list_field_pairs_marshal_too():
    """The form legacy documented — `{"fields": [[262, "req1"]]}`."""
    _send({"msg_type": "V", "fields": [[262, "req1"]]})


def test_missing_msg_type_raises():
    assert "missing 'msg_type'" in _send_error({"fields": []})


def test_missing_fields_raises():
    assert "missing 'fields'" in _send_error({"msg_type": "V"})


def test_a_wrong_length_pair_raises():
    assert "got 1 items" in _send_error({"msg_type": "V", "fields": [(262,)]})


def test_a_non_str_tag_value_raises():
    """FIX is a text protocol: a number must be spelled, never coerced."""
    message = _send_error({"msg_type": "V", "fields": [(270, 1.0842)]})
    assert "tag 270 must be a str" in message


# ---- Loopback session tests (real TCP, live clock) ----


def _statuses(ticks):
    """Flatten a burst-shaped status stream to its `status` strings."""
    return [entry["status"] for tick in ticks for entry in tick]


@pytest.mark.requires_fix
@pytest.mark.parametrize("mode", ["threaded", "spin"])
def test_acceptor_and_initiator_both_log_in(mode):
    """One graph, both sides, a real loopback socket — as `same_process` does.

    The acceptor is wired first so its listener binds before the initiator's
    synchronous `spin` connect runs: start hooks fire in wiring order.
    """
    port = free_port()
    g = wf.Graph()

    acc_data, acc_status = wf.fix_accept(
        g, port, "ACCEPTOR", "INITIATOR", poll_mode=mode
    )
    init_data, init_status = wf.fix_connect(
        g, "127.0.0.1", port, "INITIATOR", "ACCEPTOR", poll_mode=mode
    )
    acc_seen = acc_status.accumulate()
    init_seen = init_status.accumulate()
    # Keep the data streams live in the graph even though only status is asserted.
    acc_data.accumulate()
    init_data.accumulate()

    g.run(realtime=True, duration_nanos=500 * MS_NANOS)

    assert "logged_in" in _statuses(acc_seen.value())
    assert "logged_in" in _statuses(init_seen.value())


@pytest.mark.requires_fix
def test_a_message_crosses_the_session():
    """`fix_send` marshals a dict out; the acceptor's `data` decodes it back."""
    port = free_port()
    g = wf.Graph()

    data, _status = wf.fix_accept(g, port, "ACCEPTOR", "INITIATOR")
    received = data.accumulate()
    # Ticks once per cycle; the session dedupes nothing, so throttle to one send.
    wf.fix_send(
        g.constant(MESSAGE).limit(1), "127.0.0.1", port, "INITIATOR", "ACCEPTOR"
    )

    g.run(realtime=True, duration_nanos=SECOND_NANOS)

    messages = [msg for tick in received.value() for msg in tick]
    assert [m["msg_type"] for m in messages] == ["V"]
    assert messages[0]["fields"] == [(262, "req1"), (263, "1")]
    # The session stamps the sequence number; the sent dict's is ignored.
    assert messages[0]["seq_num"] >= 1


@pytest.mark.requires_fix
def test_a_refused_connection_surfaces_an_error_status():
    """A dead port yields an `error` status rather than aborting the run."""
    g = wf.Graph()
    _data, status = wf.fix_connect(g, "127.0.0.1", DEAD_PORT, "CLIENT", "SERVER")
    seen = status.accumulate()

    g.run(realtime=True, duration_nanos=SECOND_NANOS)

    entries = [entry for tick in seen.value() for entry in tick]
    assert "error" in [entry["status"] for entry in entries]
    # The carrying states keep their payload — here, why the connect failed.
    errors = [entry for entry in entries if entry["status"] == "error"]
    assert errors[0]["message"]
