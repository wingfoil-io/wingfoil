import os
import subprocess
import sys
import threading
import time

import pytest

import wingfoil as wf

if not hasattr(wf, "Iceoryx2Mode") or not hasattr(wf, "iceoryx2_sub"):
    pytest.skip(
        "iceoryx2 Python bindings are not enabled in this build (build with maturin --features iceoryx2-beta).",
        allow_module_level=True,
    )


def _unique_service_name(prefix: str) -> str:
    # Keep service names unique to avoid collisions when tests run in parallel.
    return f"wingfoil/python/test/{prefix}/{os.getpid()}/{time.time_ns()}"


@pytest.mark.parametrize(
    "mode",
    [
        wf.Iceoryx2Mode.Spin,
        wf.Iceoryx2Mode.Threaded,
        wf.Iceoryx2Mode.Signaled,
    ],
)
def test_iceoryx2_local_pubsub_bytes(mode):
    if mode == wf.Iceoryx2Mode.Spin:
        mode_name = "spin"
    elif mode == wf.Iceoryx2Mode.Threaded:
        mode_name = "threaded"
    else:
        mode_name = "signaled"

    service_name = _unique_service_name(f"local/{mode_name}")

    sub = wf.iceoryx2_sub(
        service_name,
        variant=wf.Iceoryx2ServiceVariant.Local,
        mode=mode,
        history_size=5,
    )
    collected = sub.collect()

    pub = (
        wf.ticker(0.02)
        .count()
        .map(lambda _: b"hello")
        .iceoryx2_pub(service_name, variant=wf.Iceoryx2ServiceVariant.Local)
    )

    graph = wf.Graph([pub, collected])
    graph.run(duration=0.4)

    ticks = collected.peek_value()
    assert ticks, "expected to receive at least one tick"

    values = [item for tick in ticks for item in tick]
    assert values, "expected to receive at least one message"
    assert all(v == b"hello" for v in values)


def test_iceoryx2_local_slice_large_payload():
    service_name = _unique_service_name("local/large")

    # Stay below Rust-side `initial_max_slice_len(128 * 1024)` (see slice publisher).
    payload = b"x" * (64 * 1024)

    sub = wf.iceoryx2_sub(
        service_name,
        variant=wf.Iceoryx2ServiceVariant.Local,
        mode=wf.Iceoryx2Mode.Signaled,
        history_size=5,
    )
    collected = sub.collect()

    pub = (
        wf.ticker(0.05)
        .count()
        .map(lambda _: payload)
        .iceoryx2_pub(service_name, variant=wf.Iceoryx2ServiceVariant.Local)
    )

    graph = wf.Graph([pub, collected])
    graph.run(duration=0.4)

    ticks = collected.peek_value()
    assert ticks, "expected to receive at least one tick"

    values = [item for tick in ticks for item in tick]
    assert values, "expected to receive at least one message"
    assert payload in values


@pytest.mark.skipif(
    os.getenv("WINGFOIL_ICEORYX2_IPC_TESTS") != "1",
    reason="Set WINGFOIL_ICEORYX2_IPC_TESTS=1 to enable IPC (shared memory) tests.",
)
def test_iceoryx2_ipc_pubsub_subprocess():
    if not os.path.isdir("/dev/shm") or not os.access("/dev/shm", os.W_OK):
        pytest.skip("IPC tests require a writable /dev/shm (or equivalent shared memory setup).")

    service_name = _unique_service_name("ipc/subprocess")

    sub = wf.iceoryx2_sub(
        service_name,
        variant=wf.Iceoryx2ServiceVariant.Ipc,
        mode=wf.Iceoryx2Mode.Spin,
        # Service config must match across publisher/subscriber; use an explicit value.
        history_size=10,
    )
    collected = sub.collect()

    def run_subscriber():
        wf.Graph([collected]).run(duration=1.0)

    t = threading.Thread(target=run_subscriber, daemon=True)
    t.start()

    # Give subscriber a head start.
    time.sleep(0.15)

    code = f"""
import wingfoil as wf
service_name = {service_name!r}
pub = (
    wf.ticker(0.02)
    .count()
    .map(lambda _: b"hello-ipc")
    .iceoryx2_pub(service_name, variant=wf.Iceoryx2ServiceVariant.Ipc, history_size=10)
)
wf.Graph([pub]).run(duration=0.4)
"""
    subprocess.run([sys.executable, "-c", code], check=True)

    t.join(timeout=2.0)
    ticks = collected.peek_value()
    values = [item for tick in ticks for item in tick]
    assert values, "expected to receive at least one IPC message"
    assert b"hello-ipc" in values
