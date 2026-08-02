"""Tests for the wingfoil-next Redis Python bindings.

Two groups:

- Unit-level wiring / marshaling tests (no marker): run by default, with no live
  service. They exercise the ``#[pyadapter]`` glue — argument defaults, the
  wiring-time rejection a fallible adapter surfaces as an exception, and the
  dict-to-record marshaling — without touching the network. The `redis` client
  connects lazily, so wiring against an unreachable URL is a local operation.
- Integration tests (``@pytest.mark.requires_redis``): deselected by default.
  The redis-next integration workflow opts in with ``-m requires_redis``;
  without Redis on localhost:6379 they fail loudly rather than skipping.

Local setup:
    docker run --rm -p 6379:6379 redis:7-alpine
"""

import threading
import time
import uuid

import pytest

import wingfoil_next as wf

URL = "redis://127.0.0.1:6379"
# Loopback port 1 is reserved and never hosts a service.
UNREACHABLE = "redis://127.0.0.1:1"

SECOND_NANOS = 1_000_000_000


def _name(prefix):
    """A fresh channel / stream key per test, so runs never collide."""
    return f"py-redis-{prefix}-{uuid.uuid4().hex[:12]}"


# ---- Unit-level coverage (no live Redis) ----


def test_module_exposes_the_redis_surface():
    for name in ("redis_sub", "redis_pub", "redis_stream_read", "redis_stream_write"):
        assert callable(getattr(wf, name)), name


def test_sub_constructs_a_stream():
    g = wf.Graph()
    assert isinstance(wf.redis_sub(g, UNREACHABLE, "chan"), wf.Stream)


def test_stream_read_constructs_a_stream():
    g = wf.Graph()
    assert isinstance(wf.redis_stream_read(g, UNREACHABLE, "key"), wf.Stream)


@pytest.mark.parametrize("factory", ["redis_sub", "redis_stream_read"])
def test_sources_reject_a_historical_run_at_wiring(factory):
    """Both sources are live and unbounded; the rejection is an exception."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        getattr(wf, factory)(g, UNREACHABLE, "target", realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


def test_sinks_optional_args_have_defaults():
    """`channel` / `key` and `buffer_size` are optional."""
    g = wf.Graph()
    source = g.counter(period_nanos=SECOND_NANOS)
    assert isinstance(wf.redis_pub(source, UNREACHABLE), wf.Stream)
    assert isinstance(wf.redis_stream_write(source, UNREACHABLE), wf.Stream)


def test_pub_rejects_a_non_dict_value():
    g = wf.Graph()
    wf.redis_pub(g.counter(period_nanos=SECOND_NANOS), UNREACHABLE, channel="c")
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "must be a dict" in str(excinfo.value)


def test_pub_rejects_a_record_without_a_payload():
    g = wf.Graph()
    records = g.counter(period_nanos=SECOND_NANOS).map(lambda _: {"channel": "c"})
    wf.redis_pub(records, UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "missing 'payload'" in str(excinfo.value)


def test_pub_rejects_a_record_with_no_channel_and_no_default():
    g = wf.Graph()
    records = g.counter(period_nanos=SECOND_NANOS).map(lambda _: {"payload": b"p"})
    wf.redis_pub(records, UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "no 'channel'" in str(excinfo.value)


def test_stream_write_rejects_a_record_without_fields():
    g = wf.Graph()
    records = g.counter(period_nanos=SECOND_NANOS).map(lambda _: {"key": "k"})
    wf.redis_stream_write(records, UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "missing 'fields'" in str(excinfo.value)


def test_stream_write_rejects_a_wrong_typed_field_value():
    """Legacy dropped such a field silently; here it aborts naming the field."""
    g = wf.Graph()
    records = g.counter(period_nanos=SECOND_NANOS).map(
        lambda _: {"key": "k", "fields": {"sym": 7}}
    )
    wf.redis_stream_write(records, UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "for 'sym'" in str(excinfo.value)


# ---- Integration coverage (needs Redis on localhost:6379) ----


@pytest.mark.requires_redis
def test_stream_write_then_read_round_trips():
    """XADD a batch, then read it back — the snapshot half of the reader."""
    key = _name("stream")

    # `values` replays exactly this list, one per tick — `counter` would depend
    # on how many ticks a duration bound produces, which is not under test here.
    write = wf.Graph()
    records = write.values(
        [
            {"key": key, "fields": {"seq": str(i).encode(), "sym": b"AAPL"}}
            for i in (1, 2, 3)
        ],
        period_nanos=SECOND_NANOS,
    )
    wf.redis_stream_write(records, URL)
    write.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    read = wf.Graph()
    seen = []
    wf.redis_stream_read(read, URL, key).inspect(seen.append)
    read.run(realtime=True, duration_nanos=5 * SECOND_NANOS)

    entries = [entry for tick in seen for entry in tick]
    assert 3 == len(entries)
    assert [b"1", b"2", b"3"] == [e["fields"]["seq"] for e in entries]
    assert {b"AAPL"} == {e["fields"]["sym"] for e in entries}
    assert {key} == {e["key"] for e in entries}
    # Redis assigns the entry IDs, so they are present and distinct.
    assert 3 == len({e["id"] for e in entries})


@pytest.mark.requires_redis
def test_sub_sees_messages_published_mid_run_from_another_thread():
    """The GIL-release regression test.

    ``PyGraph.run`` detaches for the duration of the run. Without that, this
    publisher thread cannot acquire the GIL until ``run`` returns — and since
    Pub/Sub replays nothing, the subscriber would see an empty stream. Every
    live source binding needs a test of this shape.
    """
    channel = _name("chan")

    def publish_later():
        # Long enough that the subscriber is running and subscribed; Pub/Sub is
        # fire-and-forget, so anything published before that is lost.
        time.sleep(3)
        g = wf.Graph()
        records = g.values(
            [
                {"channel": channel, "payload": b"live-1"},
                {"channel": channel, "payload": b"live-2"},
            ],
            period_nanos=SECOND_NANOS,
        )
        wf.redis_pub(records, URL)
        g.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    writer = threading.Thread(target=publish_later)
    writer.start()

    sub = wf.Graph()
    seen = []
    wf.redis_sub(sub, URL, channel).inspect(seen.append)
    sub.run(realtime=True, duration_nanos=12 * SECOND_NANOS)
    writer.join()

    payloads = [event["payload"] for tick in seen for event in tick]
    assert [b"live-1", b"live-2"] == payloads
    assert {channel} == {event["channel"] for tick in seen for event in tick}


@pytest.mark.requires_redis
def test_one_sink_writes_to_several_streams():
    """Each record names its own key, so the default is only a fallback."""
    key_a = _name("a")
    key_b = _name("b")

    write = wf.Graph()
    records = write.values(
        [
            {"key": key_a if i % 2 else key_b, "fields": {"seq": str(i).encode()}}
            for i in (1, 2, 3, 4)
        ],
        period_nanos=SECOND_NANOS,
    )
    wf.redis_stream_write(records, URL)
    write.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    for key, expected in ((key_a, [b"1", b"3"]), (key_b, [b"2", b"4"])):
        read = wf.Graph()
        seen = []
        wf.redis_stream_read(read, URL, key).inspect(seen.append)
        read.run(realtime=True, duration_nanos=5 * SECOND_NANOS)
        entries = [entry for tick in seen for entry in tick]
        assert expected == [e["fields"]["seq"] for e in entries]
