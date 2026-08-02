"""Tests for the wingfoil-next ZeroMQ Python bindings.

Two groups:

- Wiring and round trips (no marker): run by default. The wiring-level cases
  need no peer (a `SUB` socket connects lazily on a background thread at graph
  start), and the round trip brings up its own publisher — in the *same* graph
  as the subscriber, since ZeroMQ is peer-to-peer and needs no broker.
- etcd discovery (``@pytest.mark.requires_etcd``): the `_etcd` pair resolves the
  publisher's address through etcd, so those need one on localhost:2379. They
  are compiled only when the binding carries both the `zmq` and `etcd`
  features; `zmq-next-integration.yml` builds that pair and opts in.

`zmq_sub` is the binding that made ``#[pyadapter]`` accept a **tuple** return —
it hands back ``(data, status)``.
"""

import threading
import time

import pytest

import wingfoil_next as wf

MS_NANOS = 1_000_000
SECOND_NANOS = 1_000_000_000
ADDRESS = "tcp://127.0.0.1:5599"

ETCD_ENDPOINT = "http://127.0.0.1:2379"


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


# ---- Round trips (one process, no broker) ----


def _round_trip(port, duration_nanos=2 * SECOND_NANOS):
    """Publish a counter and subscribe to it in one graph; return what arrived.

    ZeroMQ `PUB`/`SUB` is peer-to-peer, so publisher and subscriber can be two
    nodes of the same graph. A `SUB` socket is a slow joiner — messages sent
    before it finishes connecting are simply lost — so the assertions below are
    on *what arrived*, never on the first message.
    """
    g = wf.Graph()
    counter = g.counter(period_nanos=20 * MS_NANOS)
    wf.zmq_pub(counter.map(lambda n: str(n).encode()), port, bind_address="127.0.0.1")

    data, status = wf.zmq_sub(g, f"tcp://127.0.0.1:{port}")
    received, transitions = [], []
    data.inspect(received.extend)
    status.inspect(transitions.append)

    g.run(realtime=True, duration_nanos=duration_nanos)
    return received, transitions


def test_pub_then_sub_round_trips():
    """A published `bytes` payload comes back out as `bytes`, in order."""
    received, _ = _round_trip(5601)

    assert received, "no messages reached the subscriber"
    assert all(isinstance(message, bytes) for message in received)
    # Each tick is a list of the messages that arrived between graph cycles, so
    # `received` is the flattened sequence — a consecutive run of the counter.
    numbers = [int(message) for message in received]
    assert numbers == list(range(numbers[0], numbers[0] + len(numbers)))


def test_the_status_stream_reports_connected():
    """`status` ticks on transitions only, carrying the string form."""
    _, transitions = _round_trip(5602)

    assert transitions, "no status transition observed"
    assert all(value in ("connected", "disconnected") for value in transitions)
    assert "connected" == transitions[0]


# ---- etcd discovery ----


def test_sub_etcd_rejects_an_unreachable_registry():
    """The lookup happens at wiring, so an unreachable etcd raises there —
    no run needed, and no etcd either."""
    g = wf.Graph()
    with pytest.raises(RuntimeError):
        wf.zmq_sub_etcd(g, "no-such-service", "http://127.0.0.1:59999")


@pytest.mark.requires_etcd
def test_pub_etcd_then_sub_etcd_round_trips():
    """The publisher registers its address under `name`; the subscriber
    resolves it, with the consumer side naming no port at all.

    Unlike the direct round trip this needs **two** graphs: `zmq_sub_etcd`
    resolves the address at wiring, so the publisher must already be running
    and registered. `Graph` is unsendable, so the publisher builds and runs its
    own graph entirely inside its thread.
    """
    service = "pytest/next-zmq-quotes"

    def publish():
        g = wf.Graph()
        counter = g.counter(period_nanos=20 * MS_NANOS)
        wf.zmq_pub_etcd(
            counter.map(lambda n: str(n).encode()),
            5603,
            service,
            ETCD_ENDPOINT,
            bind_address="127.0.0.1",
        )
        g.run(realtime=True, duration_nanos=3 * SECOND_NANOS)

    publisher = threading.Thread(target=publish, daemon=True)
    publisher.start()
    time.sleep(1.0)  # let it bind and register before the lookup

    g = wf.Graph()
    data, _status = wf.zmq_sub_etcd(g, service, ETCD_ENDPOINT)
    received = []
    data.inspect(received.extend)
    g.run(realtime=True, duration_nanos=SECOND_NANOS)

    publisher.join(timeout=5)

    assert received, "no messages arrived via etcd discovery"
    assert all(isinstance(message, bytes) for message in received)
