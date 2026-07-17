"""Tests for the Kafka sub/pub Python bindings.

The TestKafkaConstruction and TestKafkaUnreachable suites run by default (no
marker): they exercise the binding wiring and the dict_to_record marshaling
closure without a live broker.

The TestKafkaSub and TestKafkaPub integration suites are selected via
`-m requires_kafka` and deselected from the default run. Without a
Kafka-compatible broker on localhost:9092 they fail loudly with a real
connection error — they do NOT silently skip, so CI cannot be falsely green
against a broker that never came up.

Local setup (Redpanda):
    docker run --rm -p 9092:9092 \\
      docker.redpanda.com/redpandadata/redpanda:v24.1.1 \\
      redpanda start --overprovisioned --smp 1 --memory 512M \\
      --kafka-addr 0.0.0.0:9092 --advertise-kafka-addr localhost:9092
"""

import unittest

import pytest

BROKERS = "localhost:9092"
TOPIC_PREFIX = "wingfoil_pytest_"

# A broker address guaranteed not to accept Kafka: TCP reject on loopback.
# The producer's message.timeout.ms (5s) bounds how long a delivery waits
# before the run fails, so the marshaling-failure tests take a few seconds.
UNREACHABLE_BROKERS = "127.0.0.1:1"


class TestKafkaConstruction(unittest.TestCase):
    """Binding construction paths that don't need a running broker.

    Exercises py_kafka_sub / kafka_pub wiring, including the fluent
    .kafka_pub() stream method, without connecting.
    """

    def test_kafka_sub_constructs_stream(self):
        from wingfoil import kafka_sub

        stream = kafka_sub(UNREACHABLE_BROKERS, "topic", "group")
        self.assertIsNotNone(stream)

    def test_kafka_pub_method_constructs_node(self):
        from wingfoil import constant

        node = constant({"value": b"v"}).kafka_pub(UNREACHABLE_BROKERS, "topic")
        self.assertIsNotNone(node)

    def test_kafka_pub_from_list_constructs_node(self):
        from wingfoil import constant

        records = [{"key": b"k", "value": b"v"}, {"value": b"v2"}]
        node = constant(records).kafka_pub(UNREACHABLE_BROKERS, "topic")
        self.assertIsNotNone(node)


class TestKafkaUnreachable(unittest.TestCase):
    """Running against an unreachable broker exercises the dict_to_record
    marshaling closure (which runs on the upstream tick) before the delivery
    to the unreachable broker fails and propagates to the graph.
    """

    def test_pub_single_dict_marshals_then_errors(self):
        from wingfoil import constant

        node = constant({"topic": "t", "key": b"k", "value": b"v"}).kafka_pub(
            UNREACHABLE_BROKERS, "t"
        )
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_pub_list_of_dicts_marshals_then_errors(self):
        from wingfoil import constant

        records = [
            {"topic": "t", "key": b"k", "value": b"v"},
            {"topic": "t", "value": b"v2"},  # keyless path
        ]
        node = constant(records).kafka_pub(UNREACHABLE_BROKERS, "t")
        with self.assertRaises(Exception):
            node.run(realtime=True, cycles=1)

    def test_pub_bad_value_type_marshals_empty_burst(self):
        # Neither dict nor list: the closure logs an error and emits an empty
        # burst, so nothing is produced and the run completes without error.
        from wingfoil import constant

        constant("not a dict").kafka_pub(UNREACHABLE_BROKERS, "t").run(
            realtime=True, cycles=1
        )


@pytest.mark.requires_kafka
class TestKafkaSub(unittest.TestCase):
    def test_sub_returns_expected_shape(self):
        from wingfoil import kafka_sub

        topic = f"{TOPIC_PREFIX}sub_shape"
        stream = kafka_sub(BROKERS, topic, "pytest-sub-group").collect()
        stream.run(realtime=True, duration=3.0)
        result = stream.peek_value()
        # Nobody publishes to this topic, so the upstream may never tick. In
        # wingfoil a node that has never ticked has no value (peek_value ==
        # None); when it does tick, collect() yields a list. Either is a valid
        # "expected shape" for this smoke test.
        self.assertTrue(result is None or isinstance(result, list))

    def test_sub_dict_has_correct_fields(self):
        from wingfoil import constant, kafka_sub

        topic = f"{TOPIC_PREFIX}sub_fields"
        constant({"topic": topic, "value": b"check"}).kafka_pub(
            BROKERS, topic
        ).run(realtime=True, cycles=1)

        stream = kafka_sub(BROKERS, topic, "pytest-fields-group").collect()
        stream.run(realtime=True, duration=5.0)
        ticks = stream.peek_value()
        all_events = [e for tick in ticks for e in tick]
        self.assertTrue(all_events, "expected at least one event")
        for e in all_events:
            self.assertIn("topic", e)
            self.assertIn("partition", e)
            self.assertIn("offset", e)
            self.assertIn("key", e)
            self.assertIn("value", e)
            self.assertIsInstance(e["topic"], str)
            self.assertIsInstance(e["partition"], int)
            self.assertIsInstance(e["offset"], int)
            self.assertIsInstance(e["value"], bytes)


@pytest.mark.requires_kafka
class TestKafkaPub(unittest.TestCase):
    def test_pub_single_dict_round_trip(self):
        from wingfoil import constant, kafka_sub

        topic = f"{TOPIC_PREFIX}pub_single"
        constant({"topic": topic, "key": b"k", "value": b"hello_from_python"}).kafka_pub(
            BROKERS, topic
        ).run(realtime=True, cycles=1)

        stream = kafka_sub(BROKERS, topic, "pytest-pub-verify").collect()
        stream.run(realtime=True, duration=5.0)
        ticks = stream.peek_value()
        all_events = [e for tick in ticks for e in tick]
        values = [e["value"] for e in all_events]
        self.assertIn(b"hello_from_python", values)

    def test_pub_list_of_dicts_round_trip(self):
        from wingfoil import constant, kafka_sub

        topic = f"{TOPIC_PREFIX}pub_multi"
        entries = [
            {"topic": topic, "key": b"a", "value": b"aaa"},
            {"topic": topic, "key": b"b", "value": b"bbb"},
        ]
        constant(entries).kafka_pub(BROKERS, topic).run(realtime=True, cycles=1)

        stream = kafka_sub(BROKERS, topic, "pytest-multi-verify").collect()
        stream.run(realtime=True, duration=5.0)
        ticks = stream.peek_value()
        all_events = [e for tick in ticks for e in tick]
        values = {e["value"] for e in all_events}
        self.assertIn(b"aaa", values)
        self.assertIn(b"bbb", values)


if __name__ == "__main__":
    unittest.main()
