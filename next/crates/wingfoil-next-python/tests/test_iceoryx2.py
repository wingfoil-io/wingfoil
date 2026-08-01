"""Tests for the wingfoil-next iceoryx2 Python bindings.

Two groups:

- Wiring / selector tests (no marker): run by default. Ports are created at
  graph `start()`, not at wiring, so every entry point wires without a service
  and the argument checks are reachable on their own.
- ``@pytest.mark.requires_iceoryx2``: publish → subscribe round trips. These
  need no external service either — iceoryx2's `"local"` variant communicates
  **in-process over the heap** — but they run a live clock and touch
  `/tmp/iceoryx2` for the service registry, so they are gated the same way the
  fix loopback tier is. `iceoryx2-next-integration.yml` opts in with
  ``-m requires_iceoryx2``.

Note the binding is not in the wheel's default features (iceoryx2 is
Linux/POSIX-only), but it *is* in `all-adapters`, so a normal
``maturin develop -F all-adapters`` build has it.
"""

import os

import pytest

import wingfoil_next as wf

pytestmark = pytest.mark.skipif(
    not hasattr(wf, "iceoryx2_sub"),
    reason="built without the `iceoryx2` feature",
)

MS_NANOS = 1_000_000
SECOND_NANOS = 1_000_000_000

# Unique per process so parallel runs never share a service contract.
SERVICE = f"wingfoil/next/py/{os.getpid()}"


# ---- Wiring and selectors (no ports created yet) ----


def test_module_exposes_the_iceoryx2_surface():
    for name in ("iceoryx2_sub", "iceoryx2_pub"):
        assert callable(getattr(wf, name)), name


def test_sub_constructs_a_stream():
    g = wf.Graph()
    assert isinstance(wf.iceoryx2_sub(g, f"{SERVICE}/wire"), wf.Stream)


def test_pub_constructs_a_terminal_stream():
    g = wf.Graph()
    sink = wf.iceoryx2_pub(g.constant(b"x"), f"{SERVICE}/wire")
    assert isinstance(sink, wf.Stream)


@pytest.mark.parametrize("variant", ["ipc", "local"])
def test_both_variants_wire(variant):
    g = wf.Graph()
    assert isinstance(
        wf.iceoryx2_sub(g, f"{SERVICE}/wire", variant=variant), wf.Stream
    )


@pytest.mark.parametrize("mode", ["spin", "threaded", "signaled"])
def test_every_poll_mode_wires(mode):
    g = wf.Graph()
    assert isinstance(wf.iceoryx2_sub(g, f"{SERVICE}/wire", mode=mode), wf.Stream)


def test_an_unknown_variant_raises():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.iceoryx2_sub(g, SERVICE, variant="shm")
    assert "expected 'ipc' or 'local'" in str(excinfo.value)


def test_an_unknown_mode_raises():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.iceoryx2_sub(g, SERVICE, mode="polling")
    assert "expected 'spin', 'threaded' or 'signaled'" in str(excinfo.value)


def test_a_zero_max_slice_len_raises():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.iceoryx2_pub(g.constant(b"x"), SERVICE, initial_max_slice_len=0)
    assert "initial_max_slice_len must be at least 1" in str(excinfo.value)


def test_historical_mode_is_rejected_at_wiring():
    """A subscription is live and unbounded — there is no timeline to replay."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.iceoryx2_sub(g, SERVICE, realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


# ---- Round trips (in-process; no external service) ----


def _flatten(accumulated):
    return [item for tick in (accumulated.value() or []) for item in tick]


def _round_trip(service, payload, variant="local", mode="spin"):
    g = wf.Graph()
    received = wf.iceoryx2_sub(g, service, variant=variant, mode=mode).accumulate()
    wf.iceoryx2_pub(
        g.counter(period_nanos=20 * MS_NANOS).map(lambda _: payload),
        service,
        variant=variant,
    )
    g.run(realtime=True, duration_nanos=SECOND_NANOS)
    return _flatten(received)


@pytest.mark.requires_iceoryx2
@pytest.mark.parametrize("variant", ["local", "ipc"])
def test_a_sample_round_trips(variant):
    """The payload is sent every 20ms so a late subscriber port still sees it."""
    assert b"ping" in _round_trip(f"{SERVICE}/{variant}/one", b"ping", variant=variant)


@pytest.mark.requires_iceoryx2
def test_a_multi_sample_tick_sends_every_sample():
    """A list on one tick publishes each element as its own sample."""
    messages = _round_trip(f"{SERVICE}/local/many", [b"a", b"bb"])
    assert b"a" in messages and b"bb" in messages


@pytest.mark.requires_iceoryx2
def test_a_threaded_subscriber_receives_too():
    assert b"ping" in _round_trip(f"{SERVICE}/local/threaded", b"ping", mode="threaded")


@pytest.mark.requires_iceoryx2
def test_publishing_a_non_bytes_value_aborts_the_run():
    g = wf.Graph()
    wf.iceoryx2_pub(g.constant(42.0), f"{SERVICE}/local/bad", variant="local")
    with pytest.raises(RuntimeError):
        g.run(realtime=True, duration_nanos=200 * MS_NANOS)
