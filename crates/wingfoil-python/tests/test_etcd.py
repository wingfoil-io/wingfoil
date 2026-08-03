"""Tests for the wingfoil etcd Python bindings.

Two groups:

- Unit-level wiring / marshaling tests (no marker): run by default, with no live
  service. They exercise the ``#[pyadapter]`` glue — argument defaults, the
  wiring-time rejections a fallible adapter surfaces as exceptions, and the
  dict-to-entry marshaling — without touching the network. The etcd client
  connects lazily, so wiring against an unreachable endpoint is local.
- Integration tests (``@pytest.mark.requires_etcd``): deselected by default.
  The etcd-next integration workflow opts in with ``-m requires_etcd``; without
  etcd on localhost:2379 they fail loudly rather than skipping.

Local setup (the same image the Rust integration tests use)::

    docker run --rm -p 2379:2379 \
        -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
        -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
        gcr.io/etcd-development/etcd:v3.5.0
"""

import threading
import time
import uuid

import pytest

import wingfoil as wf

ENDPOINT = "http://localhost:2379"
# Loopback port 1 is reserved and never hosts a service.
UNREACHABLE = "http://127.0.0.1:1"

SECOND_NANOS = 1_000_000_000


def _prefix():
    """A fresh key prefix per test, so runs never see each other's keys."""
    return f"/py-etcd-{uuid.uuid4().hex[:12]}/"


# ---- Unit-level coverage (no live etcd) ----


def test_module_exposes_the_etcd_surface():
    for name in ("etcd_sub", "etcd_pub"):
        assert callable(getattr(wf, name)), name


def test_sub_constructs_a_stream():
    g = wf.Graph()
    assert isinstance(wf.etcd_sub(g, UNREACHABLE, "/p/"), wf.Stream)


def test_sub_accepts_a_list_of_endpoints():
    """The cluster form — legacy took a single endpoint only."""
    g = wf.Graph()
    stream = wf.etcd_sub(g, [UNREACHABLE, "http://127.0.0.1:2"], "/p/")
    assert isinstance(stream, wf.Stream)


def test_sub_rejects_a_bad_endpoints_argument():
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.etcd_sub(g, 7, "/p/")
    assert "must be a str or a list of str" in str(excinfo.value)

    with pytest.raises(RuntimeError) as excinfo:
        wf.etcd_sub(g, [], "/p/")
    assert "empty" in str(excinfo.value)


def test_sub_rejects_a_historical_run_at_wiring():
    """The watch is live and unbounded; the rejection is an exception."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.etcd_sub(g, UNREACHABLE, "/p/", realtime=False)
    assert "HistoricalFrom" in str(excinfo.value)


def test_pub_optional_args_have_defaults():
    g = wf.Graph()
    source = g.counter(period_nanos=SECOND_NANOS)
    assert isinstance(wf.etcd_pub(source, UNREACHABLE), wf.Stream)


def test_pub_rejects_a_nonsense_lease_ttl():
    g = wf.Graph()
    source = g.counter(period_nanos=SECOND_NANOS)
    for bad in (0.0, -1.0, float("nan")):
        with pytest.raises(RuntimeError) as excinfo:
            wf.etcd_pub(source, UNREACHABLE, lease_ttl_secs=bad)
        assert "lease_ttl_secs" in str(excinfo.value)


def test_pub_rejects_a_non_dict_value():
    g = wf.Graph()
    wf.etcd_pub(g.counter(period_nanos=SECOND_NANOS), UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "must be a dict" in str(excinfo.value)


def test_pub_rejects_an_entry_without_a_key():
    """Legacy wrote to the empty key here; this aborts instead."""
    g = wf.Graph()
    entries = g.counter(period_nanos=SECOND_NANOS).map(lambda _: {"value": b"v"})
    wf.etcd_pub(entries, UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "missing 'key'" in str(excinfo.value)


def test_pub_rejects_an_entry_without_a_value():
    g = wf.Graph()
    entries = g.counter(period_nanos=SECOND_NANOS).map(lambda _: {"key": "/p/a"})
    wf.etcd_pub(entries, UNREACHABLE)
    with pytest.raises(RuntimeError) as excinfo:
        g.run(realtime=False, start_nanos=0, duration_nanos=2 * SECOND_NANOS)
    assert "missing 'value'" in str(excinfo.value)


# ---- Integration coverage (needs etcd on localhost:2379) ----


@pytest.mark.requires_etcd
def test_pub_then_sub_reads_the_snapshot():
    """PUT a few keys, then watch the prefix — the snapshot half of the source."""
    prefix = _prefix()

    write = wf.Graph()
    entries = write.values(
        [{"key": f"{prefix}{name}", "value": name.encode()} for name in ("a", "b", "c")],
        period_nanos=SECOND_NANOS,
    )
    wf.etcd_pub(entries, ENDPOINT)
    write.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    read = wf.Graph()
    seen = []
    wf.etcd_sub(read, ENDPOINT, prefix).inspect(seen.append)
    read.run(realtime=True, duration_nanos=5 * SECOND_NANOS)

    events = [event for tick in seen for event in tick]
    assert {f"{prefix}a", f"{prefix}b", f"{prefix}c"} == {e["key"] for e in events}
    assert {b"a", b"b", b"c"} == {e["value"] for e in events}
    # Snapshot entries are always puts, and each carries a cluster revision.
    assert {"put"} == {e["kind"] for e in events}
    assert all(e["revision"] > 0 for e in events)


@pytest.mark.requires_etcd
def test_sub_sees_keys_written_mid_run_from_another_thread():
    """The GIL-release regression test.

    ``PyGraph.run`` detaches for the duration of the run. Without that, this
    writer thread cannot acquire the GIL until ``run`` returns, so the watch
    would only ever see the pre-existing snapshot. Every live source binding
    needs a test of this shape.
    """
    prefix = _prefix()

    def write_later():
        # Long enough that the watch is running and past its snapshot.
        time.sleep(3)
        g = wf.Graph()
        entries = g.values(
            [
                {"key": f"{prefix}late-1", "value": b"1"},
                {"key": f"{prefix}late-2", "value": b"2"},
            ],
            period_nanos=SECOND_NANOS,
        )
        wf.etcd_pub(entries, ENDPOINT)
        g.run(realtime=False, start_nanos=0, duration_nanos=10 * SECOND_NANOS)

    writer = threading.Thread(target=write_later)
    writer.start()

    read = wf.Graph()
    seen = []
    wf.etcd_sub(read, ENDPOINT, prefix).inspect(seen.append)
    read.run(realtime=True, duration_nanos=12 * SECOND_NANOS)
    writer.join()

    events = [event for tick in seen for event in tick]
    assert [f"{prefix}late-1", f"{prefix}late-2"] == [e["key"] for e in events]
    assert [b"1", b"2"] == [e["value"] for e in events]


@pytest.mark.requires_etcd
def test_force_false_rejects_an_existing_key():
    """The conditional-write path: a second write of the same key aborts."""
    prefix = _prefix()
    entry = [{"key": f"{prefix}once", "value": b"first"}]

    first = wf.Graph()
    wf.etcd_pub(first.values(entry, period_nanos=SECOND_NANOS), ENDPOINT)
    first.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)

    second = wf.Graph()
    wf.etcd_pub(second.values(entry, period_nanos=SECOND_NANOS), ENDPOINT, force=False)
    with pytest.raises(RuntimeError) as excinfo:
        second.run(realtime=False, start_nanos=0, duration_nanos=5 * SECOND_NANOS)
    assert f"{prefix}once" in str(excinfo.value)
