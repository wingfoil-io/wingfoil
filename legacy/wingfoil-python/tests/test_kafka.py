"""Integration tests for Kafka sub/pub Python bindings.

Selected via `-m requires_kafka`. Without a Kafka-compatible broker on
localhost:9092 the tests fail loudly with a real connection error — they
do NOT silently skip, so CI cannot be falsely green against a broker that
never came up.

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
