"""Integration tests for Kafka sub/pub Python bindings.

Requires a running Kafka-compatible broker on localhost:9092. Tests are
automatically skipped if no broker is available, making them safe to include
in a general pytest run. In CI, these are exercised by the
kafka-python-integration workflow which spins up a Redpanda service container.

Local setup (Redpanda):
    docker run --rm -p 9092:9092 \\
      docker.redpanda.com/redpandadata/redpanda:v24.1.1 \\
      redpanda start --overprovisioned --smp 1 --memory 512M \\
      --kafka-addr 0.0.0.0:9092 --advertise-kafka-addr localhost:9092
"""

import socket
import unittest

BROKERS = "localhost:9092"
TOPIC_PREFIX = "wingfoil_pytest_"


def kafka_available():
    try:
        with socket.create_connection(("localhost", 9092), timeout=1):
            return True
    except OSError:
        return False


KAFKA_AVAILABLE = kafka_available()


@unittest.skipUnless(KAFKA_AVAILABLE, "Kafka not running on localhost:9092")
class TestKafkaSub(unittest.TestCase):
    def test_sub_returns_expected_shape(self):
        from wingfoil import kafka_sub

        topic = f"{TOPIC_PREFIX}sub_shape"
        stream = kafka_sub(BROKERS, topic, "pytest-sub-group").collect()
        stream.run(realtime=True, duration=3.0)
        result = stream.peek_value()
        self.assertIsInstance(result, list)

    def test_sub_dict_has_correct_fields(self):
        from wingfoil import kafka_sub

        topic = f"{TOPIC_PREFIX}sub_fields"
        # Pre-produce a message via the pub binding
        from wingfoil import constant

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


@unittest.skipUnless(KAFKA_AVAILABLE, "Kafka not running on localhost:9092")
class TestKafkaPub(unittest.TestCase):
    def test_pub_single_dict_round_trip(self):
        from wingfoil import constant

        topic = f"{TOPIC_PREFIX}pub_single"
        constant({"topic": topic, "key": b"k", "value": b"hello_from_python"}).kafka_pub(
            BROKERS, topic
        ).run(realtime=True, cycles=1)

        # Verify via sub
        from wingfoil import kafka_sub

        stream = kafka_sub(BROKERS, topic, "pytest-pub-verify").collect()
        stream.run(realtime=True, duration=5.0)
        ticks = stream.peek_value()
        all_events = [e for tick in ticks for e in tick]
        values = [e["value"] for e in all_events]
        self.assertIn(b"hello_from_python", values)

    def test_pub_list_of_dicts_round_trip(self):
        from wingfoil import constant

        topic = f"{TOPIC_PREFIX}pub_multi"
        entries = [
            {"topic": topic, "key": b"a", "value": b"aaa"},
            {"topic": topic, "key": b"b", "value": b"bbb"},
        ]
        constant(entries).kafka_pub(BROKERS, topic).run(realtime=True, cycles=1)

        from wingfoil import kafka_sub

        stream = kafka_sub(BROKERS, topic, "pytest-multi-verify").collect()
        stream.run(realtime=True, duration=5.0)
        ticks = stream.peek_value()
        all_events = [e for tick in ticks for e in tick]
        values = {e["value"] for e in all_events}
        self.assertIn(b"aaa", values)
        self.assertIn(b"bbb", values)


if __name__ == "__main__":
    unittest.main()
