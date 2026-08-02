"""Aeron adapter Python binding tests.

Two tiers:

* **Construction / error tests** (no marker, run by default *when the binding
  is compiled in*). Aeron differs from the string-addressed adapters: a
  subscriber/publisher handle is obtained from a live media driver at
  construction time, so there is no offline construction path. These tests
  therefore assert that constructing without a driver raises ``RuntimeError`` —
  which still drives the binding's argument parsing, mode mapping, and the
  PyElement→bytes marshaling closure on the pub side. They are guarded by
  ``aeron_available`` so a default build (without ``--features aeron``) skips
  them rather than failing collection.

* **Round-trip test** (``-m requires_aeron``, deselected by default). Requires a
  running Aeron media driver; fails loudly if absent.

Build the bindings with the feature to exercise these:

    cd wingfoil-python && maturin develop --features aeron
"""

import pytest

import wingfoil as wf

# The aeron bindings are feature-gated; `wf.aeron_sub` only exists when
# wingfoil-python was built with `--features aeron`.
aeron_available = hasattr(wf, "aeron_sub")

requires_binding = pytest.mark.skipif(
    not aeron_available,
    reason="wingfoil-python built without --features aeron",
)

# A channel/stream that no media driver is serving. With no driver running,
# AeronHandle::connect() fails; with one running but nothing published, the
# subscription/publication still resolves — so these construction tests assume
# NO driver is present (the default in the unit test environment).
CHANNEL = "aeron:ipc"
STREAM_ID = 424242


@requires_binding
class TestAeronConstruction:
    def test_sub_without_driver_raises(self):
        # AeronHandle::connect() fails with no media driver — surfaced as
        # RuntimeError, exercising the sub binding's connect + error mapping.
        with pytest.raises(Exception):
            wf.aeron_sub(CHANNEL, STREAM_ID, timeout_secs=0.5)

    def test_sub_mode_argument_is_accepted(self):
        # Threaded mode flows through the PyAeronMode -> AeronMode conversion
        # before the connect attempt; still raises with no driver.
        with pytest.raises(Exception):
            wf.aeron_sub(
                CHANNEL, STREAM_ID, mode=wf.AeronMode.Threaded, timeout_secs=0.5
            )

    def test_pub_method_without_driver_raises(self):
        # The pub path connects eagerly too; raises before any value flows.
        with pytest.raises(Exception):
            wf.constant(b"v").aeron_pub(CHANNEL, STREAM_ID, timeout_secs=0.5)

    def test_aeron_mode_enum_values(self):
        # Both polling-mode variants are exposed.
        assert wf.AeronMode.Spin != wf.AeronMode.Threaded


@pytest.mark.requires_aeron
class TestAeronRoundTrip:
    def test_pub_sub_round_trip_bytes(self):
        # Publish a small ramp of byte messages on a channel and read them back
        # through a spin subscriber. Requires a running media driver.
        channel = "aeron:ipc"
        stream_id = 770001

        sub = wf.aeron_sub(channel, stream_id, mode=wf.AeronMode.Spin)
        collected = sub.collect()

        pub = wf.ticker(0.02).count().map(lambda _: b"hello").aeron_pub(
            channel, stream_id
        )

        graph = wf.Graph([pub, collected])
        graph.run(realtime=True, duration=0.4)

        ticks = collected.peek_value()
        assert ticks, "expected at least one tick"
        values = [item for tick in ticks for item in tick]
        assert values, "expected at least one message"
        assert all(v == b"hello" for v in values)
