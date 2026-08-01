"""Tests for the wingfoil-next Aeron Python bindings.

**Built only on request.** `rusteron-client` compiles the Aeron C library from
source (clang, libuuid, CMake >= 3.30), so the `aeron` feature is not in the
`all-adapters` roll-up nor in the wheel's default feature set. Every test here
skips unless the module was built with ``maturin develop -F aeron``.

Note that such a build links ``libaeron.so`` out of the cargo build directory,
which is not on the loader path — so ``LD_LIBRARY_PATH`` must point at it or
``import wingfoil_next`` itself fails::

    export LD_LIBRARY_PATH=$(dirname $(find target -name libaeron.so | head -1))

Two groups:

- Argument-validation tests (no marker): run by default when the feature is
  present. Every Aeron entry point resolves its `(channel, stream_id)` through
  the media driver at *wiring*, so nothing else is reachable without one — but
  the checks that run before that connect are, and they are the binding's own.
- ``@pytest.mark.requires_aeron``: a publish → subscribe round trip through a
  real media driver. `aeron-next-integration.yml` starts one and opts in with
  ``-m requires_aeron``; ``AERON_DIR`` points both the driver and this process
  at the same CNC file, exactly as `tests/aeron_integration.rs` does.
"""

import pytest

import wingfoil_next as wf

pytestmark = pytest.mark.skipif(
    not hasattr(wf, "aeron_sub"),
    reason="built without the `aeron` feature (maturin develop -F aeron)",
)

MS_NANOS = 1_000_000
SECOND_NANOS = 1_000_000_000

CHANNEL = "aeron:ipc"
# Distinct from every stream id the Rust integration tests use (2000-2010), so
# the two suites can share one driver without crossing streams.
STREAM_ID = 3001


# ---- Argument validation (runs before the driver is touched) ----


def test_module_exposes_the_aeron_surface():
    for name in ("aeron_sub", "aeron_sub_with_status", "aeron_pub", "aeron_pub_with_status"):
        assert callable(getattr(wf, name)), name


def test_an_unknown_mode_raises():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.aeron_sub(g, CHANNEL, STREAM_ID, mode="polling")
    assert "expected 'spin' or 'threaded'" in str(excinfo.value)


def test_a_zero_fragment_limit_raises():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.aeron_sub(g, CHANNEL, STREAM_ID, fragment_limit=0)
    assert "fragment_limit must be at least 1" in str(excinfo.value)


@pytest.mark.parametrize("timeout", [0.0, -1.0, float("nan")])
def test_a_nonsense_timeout_raises(timeout):
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.aeron_sub(g, CHANNEL, STREAM_ID, timeout_secs=timeout)
    assert "timeout_secs" in str(excinfo.value)


def test_historical_mode_is_rejected_before_the_driver_is_touched():
    """A subscription is live and unbounded — and this must not try to connect."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.aeron_sub(g, CHANNEL, STREAM_ID, realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


def test_with_status_validates_its_arguments_too():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.aeron_sub_with_status(g, CHANNEL, STREAM_ID, realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


# ---- Round trip through a real media driver ----


def _flatten(accumulated):
    return [item for tick in (accumulated.value() or []) for item in tick]


@pytest.mark.requires_aeron
@pytest.mark.parametrize("mode", ["spin", "threaded"])
def test_a_message_round_trips_over_ipc(mode):
    """Publish and subscribe in one graph over `aeron:ipc`.

    The subscription is wired first so its image is established before the
    publication joins, and the payload is sent every 50ms for the whole run —
    an Aeron publication silently drops offers until the driver conductor has
    linked the two, which is the dominant source of flakiness here.
    """
    g = wf.Graph()
    stream_id = STREAM_ID + (0 if mode == "spin" else 1)

    received = wf.aeron_sub(g, CHANNEL, stream_id, mode=mode).accumulate()
    wf.aeron_pub(
        g.counter(period_nanos=50 * MS_NANOS).map(lambda _: b"ping"),
        CHANNEL,
        stream_id,
    )

    g.run(realtime=True, duration_nanos=2 * SECOND_NANOS)

    assert b"ping" in _flatten(received)


@pytest.mark.requires_aeron
def test_a_multi_message_tick_sends_every_message():
    """A list on one tick publishes each element as its own Aeron message."""
    g = wf.Graph()
    stream_id = STREAM_ID + 2

    received = wf.aeron_sub(g, CHANNEL, stream_id).accumulate()
    wf.aeron_pub(
        g.counter(period_nanos=50 * MS_NANOS).map(lambda _: [b"a", b"bb"]),
        CHANNEL,
        stream_id,
    )

    g.run(realtime=True, duration_nanos=2 * SECOND_NANOS)

    messages = _flatten(received)
    assert b"a" in messages and b"bb" in messages


@pytest.mark.requires_aeron
def test_the_status_streams_report_connected():
    """Both `_with_status` twins tick a transition once the image is linked."""
    g = wf.Graph()
    stream_id = STREAM_ID + 3

    _data, sub_status = wf.aeron_sub_with_status(g, CHANNEL, stream_id)
    sub_seen = sub_status.accumulate()
    _sink, pub_status = wf.aeron_pub_with_status(
        g.counter(period_nanos=50 * MS_NANOS).map(lambda _: b"ping"),
        CHANNEL,
        stream_id,
    )
    pub_seen = pub_status.accumulate()

    g.run(realtime=True, duration_nanos=2 * SECOND_NANOS)

    assert "connected" in _flatten(sub_seen)
    assert "connected" in _flatten(pub_seen)


@pytest.mark.requires_aeron
def test_publishing_a_non_bytes_value_aborts_the_run():
    """Legacy published an empty message here; this fails loudly instead."""
    g = wf.Graph()
    wf.aeron_pub(g.constant(42.0), CHANNEL, STREAM_ID + 4)

    with pytest.raises(RuntimeError):
        g.run(realtime=True, duration_nanos=200 * MS_NANOS)
