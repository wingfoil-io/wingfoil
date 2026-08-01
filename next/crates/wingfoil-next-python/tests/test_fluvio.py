"""Tests for the wingfoil-next Fluvio Python bindings.

Two groups:

- Unit-level wiring / marshaling tests (no marker): run by default, with no live
  service. They exercise the ``#[pyadapter]`` glue — argument defaults, the
  wiring-time rejections a fallible adapter surfaces as exceptions, and the
  dict-to-record marshaling. The Fluvio client connects at run start, so wiring
  against an unreachable SC is a local operation.
- Integration tests (``@pytest.mark.requires_fluvio``): deselected by default.

.. warning::

   **The integration group is not yet wired into CI, and has never been run.**
   Unlike redis / etcd / kafka, a Fluvio cluster cannot be brought up from bash:
   the SC must be told about the SPU through ``FluvioAdmin`` (a Rust API) before
   the SPU process connects, which is why the Rust integration test carries a
   ~100-line ``FluvioCluster`` helper using testcontainers with host networking
   on fixed ports. Wiring an equivalent for the Python leg is tracked as
   follow-up work; until then these tests document the intended behaviour and
   are runnable by hand against a cluster on ``localhost:9003`` with the topic
   already created::

       fluvio topic create py-fluvio-test
       pytest -m requires_fluvio tests/test_fluvio.py
"""

import threading
import time

import pytest

import wingfoil_next as wf

ENDPOINT = "127.0.0.1:9003"
# Loopback port 1 is reserved and never hosts a service.
UNREACHABLE = "127.0.0.1:1"

# Fluvio topics must exist before use, so integration tests share one rather
# than minting a fresh name per test the way the other adapters do.
TOPIC = "py-fluvio-test"

SECOND_NANOS = 1_000_000_000


# ---- Unit-level coverage (no live Fluvio) ----


def test_module_exposes_the_fluvio_surface():
    for name in ("fluvio_sub", "fluvio_pub"):
        assert callable(getattr(wf, name)), name


def test_sub_constructs_a_stream():
    g = wf.Graph()
    assert isinstance(wf.fluvio_sub(g, UNREACHABLE, "topic"), wf.Stream)


def test_sub_optional_args_have_defaults():
    """`partition` / `start_offset` / `realtime` are optional."""
    g = wf.Graph()
    assert isinstance(wf.fluvio_sub(g, UNREACHABLE, "topic"), wf.Stream)
    assert isinstance(
        wf.fluvio_sub(g, UNREACHABLE, "topic", partition=2, start_offset=10),
        wf.Stream,
    )


def test_sub_rejects_a_negative_start_offset_at_wiring():
    """Pure validation, so the adapter fails fast rather than at run start."""
    g = wf.Graph()
    with pytest.raises(RuntimeError):
        wf.fluvio_sub(g, UNREACHABLE, "topic", start_offset=-1)


def test_sub_rejects_a_historical_run_at_wiring():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.fluvio_sub(g, UNREACHABLE, "topic", realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


def test_pub_constructs_a_terminal_stream():
    g = wf.Graph()
    source = g.counter(period_nanos=SECOND_NANOS)
    assert isinstance(wf.fluvio_pub(source, UNREACHABLE, "topic"), wf.Stream)


def test_pub_rejects_a_non_dict_value():
    g = wf.Graph()
    wf.fluvio_pub(g.counter(period_nanos=SECOND_NANOS), UNREACHABLE, "topic")
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "must be a dict" in str(excinfo.value)


def test_pub_rejects_a_record_without_a_value():
    g = wf.Graph()
    records = g.counter(period_nanos=SECOND_NANOS).map(lambda _: {"key": "k"})
    wf.fluvio_pub(records, UNREACHABLE, "topic")
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "missing 'value'" in str(excinfo.value)


def test_pub_rejects_a_bytes_key():
    """The read/write key asymmetry, made explicit.

    A consumed key reads back as ``bytes`` but a produced key is a ``str``
    (Fluvio's ``RecordKey`` is textual), so handing a consumed key straight back
    is an error rather than a silently keyless send. Round-tripping needs
    ``.decode()``.
    """
    g = wf.Graph()
    records = g.counter(period_nanos=SECOND_NANOS).map(
        lambda _: {"key": b"raw", "value": b"v"}
    )
    wf.fluvio_pub(records, UNREACHABLE, "topic")
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "'key' must be a str" in str(excinfo.value)


# ---- Integration coverage (needs a Fluvio cluster on localhost:9003) ----
# See the module docstring: not yet wired into CI, never executed.


@pytest.mark.requires_fluvio
def test_pub_then_sub_round_trips():
    """Produce a batch, then consume it back from the start of the partition."""
    payloads = [b"alpha", b"beta", b"gamma"]

    produce = wf.Graph()
    records = produce.values(
        [{"key": "k", "value": p} for p in payloads], period_nanos=SECOND_NANOS
    )
    wf.fluvio_pub(records, ENDPOINT, TOPIC)
    produce.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    consume = wf.Graph()
    seen = []
    wf.fluvio_sub(consume, ENDPOINT, TOPIC, start_offset=0).inspect(seen.append)
    consume.run(realtime=True, duration_nanos=15 * SECOND_NANOS)

    events = [event for tick in seen for event in tick]
    # The topic is shared across runs, so assert the batch is a suffix rather
    # than the whole partition.
    assert payloads == [e["value"] for e in events][-len(payloads) :]
    assert all(e["offset"] >= 0 for e in events)


@pytest.mark.requires_fluvio
def test_sub_sees_records_produced_mid_run_from_another_thread():
    """The GIL-release regression test.

    ``PyGraph.run`` detaches for the duration of the run. Without that, this
    producer thread cannot acquire the GIL until ``run`` returns, so the
    consumer would only ever see records that already existed.
    """

    def produce_later():
        time.sleep(3)
        g = wf.Graph()
        records = g.values(
            [{"value": b"late-1"}, {"value": b"late-2"}], period_nanos=SECOND_NANOS
        )
        wf.fluvio_pub(records, ENDPOINT, TOPIC)
        g.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    writer = threading.Thread(target=produce_later)
    writer.start()

    # Start from the current end of the partition so only the mid-run records
    # are seen, not the topic's history.
    consume = wf.Graph()
    seen = []
    wf.fluvio_sub(consume, ENDPOINT, TOPIC).inspect(seen.append)
    consume.run(realtime=True, duration_nanos=20 * SECOND_NANOS)
    writer.join()

    events = [event for tick in seen for event in tick]
    assert [b"late-1", b"late-2"] == [e["value"] for e in events][-2:]
    # Both were sent without a key, so they read back keyless.
    assert [None, None] == [e["key"] for e in events][-2:]
