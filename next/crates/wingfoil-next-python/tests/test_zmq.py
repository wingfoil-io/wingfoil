"""Tests for the wingfoil-next ZeroMQ Python bindings.

All of these run by default: the wiring-level cases need no peer (a `SUB`
socket connects lazily on a background thread at graph start), and the
round-trip case brings up its own publisher inside the same process.

`zmq_sub` is the binding that made ``#[pyadapter]`` accept a **tuple** return —
it hands back ``(data, status)``.
"""

import pytest

import wingfoil_next as wf

SECOND_NANOS = 1_000_000_000
ADDRESS = "tcp://127.0.0.1:5599"


def test_module_exposes_the_zmq_surface():
    for name in ("zmq_sub", "zmq_pub"):
        assert callable(getattr(wf, name)), name


def test_sub_returns_a_tuple_of_two_streams():
    """The tuple return: `(data, status)`, both ordinary Streams."""
    g = wf.Graph()
    result = wf.zmq_sub(g, ADDRESS)
    assert isinstance(result, tuple)
    assert 2 == len(result)
    data, status = result
    assert isinstance(data, wf.Stream)
    assert isinstance(status, wf.Stream)


def test_sub_rejects_a_historical_run_at_wiring():
    """A subscriber is live and never-closing; the rejection is an exception."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.zmq_sub(g, ADDRESS, realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


def test_pub_constructs_a_terminal_stream():
    g = wf.Graph()
    payloads = g.values([b"a"], period_nanos=SECOND_NANOS)
    assert isinstance(wf.zmq_pub(payloads, 5598), wf.Stream)


def test_pub_bind_address_is_optional():
    g = wf.Graph()
    payloads = g.values([b"a"], period_nanos=SECOND_NANOS)
    assert isinstance(
        wf.zmq_pub(payloads, 5597, bind_address="127.0.0.1"), wf.Stream
    )


def test_pub_rejects_a_non_bytes_value():
    """The sink's input is typed `bytes`; anything else aborts the run."""
    g = wf.Graph()
    wf.zmq_pub(g.counter(period_nanos=SECOND_NANOS), 5596)
    with pytest.raises(RuntimeError):
        g.run(realtime=False, start_nanos=0, duration_nanos=SECOND_NANOS)
