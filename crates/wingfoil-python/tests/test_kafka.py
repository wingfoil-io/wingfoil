"""Tests for the wingfoil Kafka Python bindings.

Two groups:

- Unit-level wiring / marshaling tests (no marker): run by default, with no live
  service. They exercise the ``#[pyadapter]`` glue — argument defaults, the
  wiring-time rejection a fallible adapter surfaces as an exception, and the
  dict-to-record marshaling — without touching the network. librdkafka connects
  lazily, so wiring against an unreachable broker is a local operation.
- Integration tests (``@pytest.mark.requires_kafka``): deselected by default.
  The kafka-next integration workflow opts in with ``-m requires_kafka``;
  without a broker on localhost:9092 they fail loudly rather than skipping.

Local setup:
    docker run --rm -p 9092:9092 \
        docker.redpanda.com/redpandadata/redpanda:v24.1.1 \
        redpanda start --overprovisioned --smp 1 --memory 1G \
        --kafka-addr PLAINTEXT://0.0.0.0:9092 \
        --advertise-kafka-addr PLAINTEXT://localhost:9092
"""

import threading
import time
import uuid

import pytest

import wingfoil as wf

BROKERS = "localhost:9092"
# librdkafka only connects on the first produce/poll, so an unreachable broker
# is fine for the wiring-level tests.
UNREACHABLE = "127.0.0.1:1"

SECOND_NANOS = 1_000_000_000


def _topic():
    """A fresh topic per test, so runs never see each other's messages."""
    return f"py-kafka-{uuid.uuid4().hex[:12]}"


# ---- Unit-level coverage (no live Kafka) ----


def test_module_exposes_the_kafka_surface():
    for name in ("kafka_sub", "kafka_pub"):
        assert callable(getattr(wf, name)), name


def test_sub_constructs_a_stream():
    g = wf.Graph()
    stream = wf.kafka_sub(g, UNREACHABLE, "topic", "group")
    assert isinstance(stream, wf.Stream)


def test_sub_realtime_defaults_to_true():
    """`realtime` is optional — the forwarded pyo3 signature."""
    g = wf.Graph()
    assert isinstance(wf.kafka_sub(g, UNREACHABLE, "topic", "group"), wf.Stream)


def test_sub_rejects_a_historical_run_at_wiring():
    """A live consumer has no timeline to replay; the rejection is an exception."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.kafka_sub(g, UNREACHABLE, "topic", "group", realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


def test_pub_constructs_a_terminal_stream():
    g = wf.Graph()
    source = g.counter(period_nanos=SECOND_NANOS)
    assert isinstance(wf.kafka_pub(source, UNREACHABLE, topic="t"), wf.Stream)


def test_pub_topic_is_optional():
    g = wf.Graph()
    source = g.counter(period_nanos=SECOND_NANOS)
    assert isinstance(wf.kafka_pub(source, UNREACHABLE), wf.Stream)


def test_pub_rejects_a_non_dict_value():
    """Marshaling failures abort the run rather than publishing nothing."""
    g = wf.Graph()
    # A counter ticks ints, not record dicts.
    wf.kafka_pub(g.counter(period_nanos=SECOND_NANOS), UNREACHABLE, topic="t")
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "must be a dict" in str(excinfo.value)


def test_pub_rejects_a_record_without_a_value():
    g = wf.Graph()
    records = g.counter(period_nanos=SECOND_NANOS).map(lambda _: {"topic": "t"})
    wf.kafka_pub(records, UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "missing 'value'" in str(excinfo.value)


def test_pub_rejects_a_record_with_no_topic_and_no_default():
    g = wf.Graph()
    records = g.counter(period_nanos=SECOND_NANOS).map(lambda _: {"value": b"v"})
    wf.kafka_pub(records, UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "no 'topic'" in str(excinfo.value)


# ---- Integration coverage (needs a broker on localhost:9092) ----


@pytest.mark.requires_kafka
def test_pub_then_sub_round_trips():
    """Publish a batch, then consume it back as dicts of bytes."""
    topic = _topic()
    group = f"g-{uuid.uuid4().hex[:8]}"

    payloads = [b"alpha", b"beta", b"gamma"]

    # `values` replays exactly this list, one per tick — `counter` would depend
    # on how many ticks a duration bound produces, which is not what is under
    # test here.
    produce = wf.Graph()
    records = produce.values(
        [{"topic": topic, "key": b"k", "value": p} for p in payloads],
        period_nanos=SECOND_NANOS,
    )
    wf.kafka_pub(records, BROKERS)
    produce.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    # A fresh group reads from the beginning, so the batch above is replayed.
    consume = wf.Graph()
    seen = []
    wf.kafka_sub(consume, BROKERS, topic, group).inspect(seen.append)
    consume.run(realtime=True, duration_nanos=15 * SECOND_NANOS)

    events = [event for tick in seen for event in tick]
    assert payloads == [e["value"] for e in events]
    assert {b"k"} == {e["key"] for e in events}
    assert {topic} == {e["topic"] for e in events}


@pytest.mark.requires_kafka
def test_sub_sees_messages_produced_mid_run_from_another_thread():
    """The GIL-release regression test.

    ``PyGraph.run`` detaches for the duration of the run. Without that, this
    producer thread cannot acquire the GIL until ``run`` returns, so the
    consumer would only ever see messages that already existed. Every live
    source binding needs a test of this shape.
    """
    topic = _topic()
    group = f"g-{uuid.uuid4().hex[:8]}"

    def produce_later():
        # Long enough that the consumer is running and subscribed.
        time.sleep(3)
        g = wf.Graph()
        records = g.values(
            [{"topic": topic, "value": b"late-1"}, {"topic": topic, "value": b"late-2"}],
            period_nanos=SECOND_NANOS,
        )
        wf.kafka_pub(records, BROKERS)
        g.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    writer = threading.Thread(target=produce_later)
    writer.start()

    consume = wf.Graph()
    seen = []
    wf.kafka_sub(consume, BROKERS, topic, group).inspect(seen.append)
    consume.run(realtime=True, duration_nanos=20 * SECOND_NANOS)
    writer.join()

    values = [event["value"] for tick in seen for event in tick]
    assert [b"late-1", b"late-2"] == values


@pytest.mark.requires_kafka
def test_a_record_without_a_key_round_trips_as_none():
    topic = _topic()
    group = f"g-{uuid.uuid4().hex[:8]}"

    produce = wf.Graph()
    records = produce.values(
        [{"topic": topic, "value": b"no-key"}], period_nanos=SECOND_NANOS
    )
    wf.kafka_pub(records, BROKERS)
    produce.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    consume = wf.Graph()
    seen = []
    wf.kafka_sub(consume, BROKERS, topic, group).inspect(seen.append)
    consume.run(realtime=True, duration_nanos=15 * SECOND_NANOS)

    events = [event for tick in seen for event in tick]
    assert 1 == len(events)
    assert events[0]["key"] is None
    assert b"no-key" == events[0]["value"]
