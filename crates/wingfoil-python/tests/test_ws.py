"""Tests for the wingfoil ws Python bindings.

**One group — everything runs by default, with no marker and no extra
dependency.** There is no external service: the round trips run against a
~70-line WebSocket server built on `socket` from the standard library
(`_MiniWsServer` below).

That is deliberate, and it is why this file has no `requires_ws` marker while
`test_web.py` has `requires_web`. The `web` binding *is* a server, so testing it
needs a WebSocket **client**, and writing one means masking, continuation
frames and close handshakes — hence the `websockets` package. Here the binding
is the client, so the test only has to be a server, and a server that sends
short unmasked text frames to one well-behaved client is small enough to write
against the socket API. Speaking the protocol for setup only, and routing every
value assertion back through the adapter under test, is the same tactic
`test_kdb.py`'s `_q` helper uses.
"""

import base64
import gc
import hashlib
import socket
import threading
import time

import pytest

import wingfoil as wf

MS_NANOS = 1_000_000

# RFC 6455's handshake constant.
_WS_GUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

# Long enough for a connect plus a couple of frames on a loaded runner, short
# enough that a whole file of these stays quick.
RUN_NANOS = 700 * MS_NANOS

# How long one poll of the client socket waits before the serve loop goes round
# again. Short, because a long one would block publishing past the end of a
# sub-second run.
POLL_TIMEOUT = 0.02

# How long to keep reading client frames before publishing, so a subscription
# is recorded before a `close_after_send` server hangs up.
SUBSCRIBE_GRACE = 0.1

# Sentinel distinguishing "peer closed" from "nothing yet".
_CLOSED = object()


class _MiniWsServer:
    """A minimal WebSocket server: enough of RFC 6455 to talk to one client.

    Handles exactly what these tests need — a handshake, unmasked server→client
    text and binary frames under 126 bytes, and masked client→server text
    frames. Not a general implementation: no continuation frames, no
    fragmentation, no close handshake.
    """

    def __init__(self, send=(), close_after_send=False):
        self._send = list(send)
        self._close_after_send = close_after_send
        self.received = []
        self.connections = 0
        self._lock = threading.Lock()
        self._stop = threading.Event()
        self._connection_threads = []

        self._listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._listener.bind(("127.0.0.1", 0))
        self._listener.listen(8)
        self._listener.settimeout(0.05)
        self.port = self._listener.getsockname()[1]

        self._thread = threading.Thread(target=self._accept_loop, daemon=True)
        self._thread.start()

    @property
    def url(self):
        return f"ws://127.0.0.1:{self.port}/stream"

    def stop(self):
        """Shut down and **join every server thread**.

        Joining matters beyond tidiness: a `Graph` is an `unsendable` pyclass,
        so if a garbage collection happens to run on a lingering daemon thread
        while it still holds the last reference, pyo3 raises
        "unsendable, but is being dropped on another thread". Leaving no
        server threads running keeps collection on the main thread.
        """
        self._stop.set()
        self._thread.join(timeout=2)
        with self._lock:
            threads = list(self._connection_threads)
        for thread in threads:
            thread.join(timeout=2)
        self._listener.close()

    def __enter__(self):
        return self

    def __exit__(self, *_):
        self.stop()

    def snapshot(self):
        """The frames received and the connection count, read atomically."""
        with self._lock:
            return list(self.received), self.connections

    # -- internals ---------------------------------------------------------

    def _accept_loop(self):
        while not self._stop.is_set():
            try:
                conn, _ = self._listener.accept()
            except socket.timeout:
                continue
            except OSError:
                return
            thread = threading.Thread(target=self._serve, args=(conn,), daemon=True)
            with self._lock:
                self.connections += 1
                self._connection_threads.append(thread)
            thread.start()

    def _serve(self, conn):
        try:
            # The handshake arrives immediately on connect, so it gets a
            # generous budget; everything after it polls in short slices,
            # because a long socket timeout here would block publishing past
            # the end of a sub-second test run.
            conn.settimeout(0.5)
            if not self._handshake(conn):
                return

            conn.settimeout(POLL_TIMEOUT)
            # Give the client a moment to finish sending its subscriptions, so
            # they are recorded before we publish (and possibly hang up).
            deadline = time.monotonic() + SUBSCRIBE_GRACE
            while time.monotonic() < deadline:
                if not self._drain_one(conn):
                    return

            for payload in self._send:
                conn.sendall(self._encode(payload))

            if self._close_after_send:
                return

            while not self._stop.is_set():
                if not self._drain_one(conn):
                    return
        except OSError:
            return
        finally:
            conn.close()

    def _drain_one(self, conn):
        """Record one client frame if there is one. False once the peer hangs up."""
        frame = self._read_frame(conn)
        if frame is _CLOSED:
            return False
        if frame is not None:
            with self._lock:
                self.received.append(frame)
        return True

    def _handshake(self, conn):
        request = b""
        while b"\r\n\r\n" not in request:
            try:
                chunk = conn.recv(4096)
            except socket.timeout:
                return False
            if not chunk:
                return False
            request += chunk

        key = None
        for line in request.decode("latin-1").split("\r\n"):
            if line.lower().startswith("sec-websocket-key:"):
                key = line.split(":", 1)[1].strip()
        if key is None:
            return False

        accept = base64.b64encode(
            hashlib.sha1((key + _WS_GUID).encode()).digest()
        ).decode()
        conn.sendall(
            (
                "HTTP/1.1 101 Switching Protocols\r\n"
                "Upgrade: websocket\r\n"
                "Connection: Upgrade\r\n"
                f"Sec-WebSocket-Accept: {accept}\r\n\r\n"
            ).encode()
        )
        return True

    @staticmethod
    def _encode(payload):
        """One unmasked server→client frame, text or binary, under 126 bytes."""
        if isinstance(payload, str):
            opcode, body = 0x81, payload.encode()
        else:
            opcode, body = 0x82, bytes(payload)
        assert len(body) < 126, "the mini server only writes short frames"
        return bytes([opcode, len(body)]) + body

    def _read_frame(self, conn):
        """One masked client→server text frame.

        `None` means "nothing yet" (the poll timed out); `_CLOSED` means the
        peer hung up. Distinguishing them matters — treating a close as a
        timeout spins the serve loop on a dead socket.
        """
        try:
            header = self._recv_exactly(conn, 2)
            if header is None:
                return _CLOSED
            length = header[1] & 0x7F
            masked = header[1] & 0x80
            if length >= 126:
                return _CLOSED
            mask = self._recv_exactly(conn, 4) if masked else b"\x00\x00\x00\x00"
            if mask is None:
                return _CLOSED
            body = self._recv_exactly(conn, length) if length else b""
            if body is None:
                return _CLOSED
            unmasked = bytes(b ^ mask[i % 4] for i, b in enumerate(body))
            return unmasked.decode("utf-8", "replace")
        except socket.timeout:
            return None

    @staticmethod
    def _recv_exactly(conn, count):
        buffer = b""
        while len(buffer) < count:
            chunk = conn.recv(count - len(buffer))
            if not chunk:
                return None
            buffer += chunk
        return buffer


@pytest.fixture(autouse=True)
def _collect_graphs_on_the_main_thread():
    """Collect each test's `Graph` here, on the main thread.

    `Graph` is an `unsendable` pyclass, and a graph wired to streams is part of
    a reference cycle — so it is freed by the *cyclic* collector, which runs on
    whichever thread happens to trigger it. Left alone, one test's garbage
    graph gets collected on a later test's server thread, and pyo3 raises
    "unsendable, but is being dropped on another thread". Collecting between
    tests, before the next server starts, keeps that on the main thread.
    """
    yield
    gc.collect()


def _flat(ticks):
    """Every frame across all ticks — each tick is a burst list.

    `accumulate().value()` is `None` when the stream never ticked, which is a
    legitimate outcome for a realtime run that connected too slowly.
    """
    return [frame for tick in (ticks or []) for frame in tick]


# ---------------------------------------------------------------------------
# Surface and wiring-time rejections — no server needed
# ---------------------------------------------------------------------------


def test_module_exposes_the_ws_names():
    assert hasattr(wf, "ws_sub")
    assert hasattr(wf, "WsConnection")


def test_wiring_returns_a_stream():
    g = wf.Graph()
    stream = wf.ws_sub(g, "ws://127.0.0.1:1/stream", max_attempts=1)
    assert isinstance(stream, wf.Stream)


def test_a_historical_run_mode_raises():
    g = wf.Graph()
    with pytest.raises(RuntimeError, match="HistoricalFrom is unsupported"):
        wf.ws_sub(g, "ws://127.0.0.1:1/stream", realtime=False)


def test_a_non_websocket_url_raises():
    g = wf.Graph()
    with pytest.raises(RuntimeError, match="not a WebSocket URL"):
        wf.ws_sub(g, "https://example.com/stream")


def test_a_bad_subscription_type_raises():
    g = wf.Graph()
    with pytest.raises(RuntimeError, match="str"):
        wf.ws_sub(g, "ws://127.0.0.1:1/stream", subscriptions=[{"op": "subscribe"}])


@pytest.mark.parametrize("bad", [-1.0, float("nan"), float("inf")])
def test_a_bad_timeout_raises(bad):
    g = wf.Graph()
    with pytest.raises(RuntimeError, match="idle_timeout_secs"):
        wf.ws_sub(g, "ws://127.0.0.1:1/stream", idle_timeout_secs=bad)


def test_wiring_errors_do_not_leak_credentials():
    """A traceback is exactly where a pasted URL with an embedded key ends up."""
    g = wf.Graph()
    with pytest.raises(RuntimeError) as excinfo:
        wf.ws_sub(g, "https://user:hunter2@example.com/ws?api_key=abc123")
    message = str(excinfo.value)
    assert "hunter2" not in message
    assert "abc123" not in message


def test_the_connection_handle_exposes_its_three_parts():
    g = wf.Graph()
    conn = wf.WsConnection(g, "ws://127.0.0.1:1/stream", max_attempts=1)
    assert isinstance(conn.messages, wf.Stream)
    assert isinstance(conn.status, wf.Stream)
    assert isinstance(conn.send_stream(g.constant("ping")), wf.Stream)


def test_the_connection_rejects_a_historical_run_mode():
    g = wf.Graph()
    with pytest.raises(RuntimeError, match="HistoricalFrom is unsupported"):
        wf.WsConnection(g, "ws://127.0.0.1:1/stream", realtime=False)


# ---------------------------------------------------------------------------
# Round trips against the mini server
# ---------------------------------------------------------------------------


def test_frames_arrive_as_lists_of_str():
    with _MiniWsServer(send=["first", "second"]) as server:
        g = wf.Graph()
        seen = wf.ws_sub(g, server.url, backoff_initial_secs=0.02).accumulate()
        g.run(realtime=True, duration_nanos=RUN_NANOS)

    ticks = seen.value() or []
    assert all(isinstance(tick, list) for tick in ticks), (
        f"every tick must be a burst list, got {ticks}"
    )
    assert _flat(ticks) == ["first", "second"]


def test_a_binary_frame_arrives_as_bytes():
    with _MiniWsServer(send=[b"\x00\x01\x02"]) as server:
        g = wf.Graph()
        seen = wf.ws_sub(g, server.url, backoff_initial_secs=0.02).accumulate()
        g.run(realtime=True, duration_nanos=RUN_NANOS)

    frames = _flat(seen.value())
    assert frames == [b"\x00\x01\x02"]
    assert isinstance(frames[0], bytes), "a binary frame must not arrive as a list of ints"


def test_subscriptions_are_sent_on_connect():
    with _MiniWsServer(send=["ack"]) as server:
        g = wf.Graph()
        wf.ws_sub(
            g,
            server.url,
            subscriptions=['{"op":"subscribe","args":["a"]}', "second"],
            backoff_initial_secs=0.02,
        ).accumulate()
        g.run(realtime=True, duration_nanos=RUN_NANOS)

    received, _ = server.snapshot()
    assert received == ['{"op":"subscribe","args":["a"]}', "second"]


def test_subscriptions_are_resent_after_a_reconnect():
    """The reason the adapter exists: a reconnect must restore the feed."""
    with _MiniWsServer(send=["hello"], close_after_send=True) as server:
        g = wf.Graph()
        wf.ws_sub(
            g,
            server.url,
            subscriptions=["SUBSCRIBE"],
            backoff_initial_secs=0.02,
            jitter=False,
        ).accumulate()
        g.run(realtime=True, duration_nanos=RUN_NANOS)

    received, connections = server.snapshot()
    assert connections >= 2, f"expected a reconnect, saw {connections} connection(s)"
    assert received.count("SUBSCRIBE") >= 2, (
        f"every connection must re-send the subscription, got {received}"
    )


def test_status_reports_connected_then_reconnecting():
    with _MiniWsServer(send=["one"], close_after_send=True) as server:
        g = wf.Graph()
        conn = wf.WsConnection(
            g, server.url, backoff_initial_secs=0.02, jitter=False
        )
        statuses = conn.status.accumulate()
        conn.messages.accumulate()
        g.run(realtime=True, duration_nanos=RUN_NANOS)

    seen = statuses.value() or []
    assert all(isinstance(s, dict) for s in seen), f"status must be dicts, got {seen}"
    assert seen[0] == {"state": "connected"}
    reconnects = [s for s in seen if s["state"] == "reconnecting"]
    assert reconnects, f"a dropped connection should report reconnecting, got {seen}"
    assert reconnects[0]["attempt"] == 1, (
        f"the retry count must survive the dict edge, got {reconnects[0]}"
    )


def test_send_delivers_a_frame_queued_before_the_run():
    with _MiniWsServer(send=["ack"]) as server:
        g = wf.Graph()
        conn = wf.WsConnection(g, server.url, backoff_initial_secs=0.02)
        conn.messages.accumulate()
        conn.send("queued-early")
        g.run(realtime=True, duration_nanos=RUN_NANOS)

    received, _ = server.snapshot()
    assert "queued-early" in received


def test_send_stream_delivers_graph_values():
    with _MiniWsServer(send=["ack"]) as server:
        g = wf.Graph()
        conn = wf.WsConnection(g, server.url, backoff_initial_secs=0.02)
        conn.messages.accumulate()
        conn.send_stream(g.counter(period_nanos=50 * MS_NANOS).map(lambda _: "tick"))
        g.run(realtime=True, duration_nanos=RUN_NANOS)

    received, _ = server.snapshot()
    assert "tick" in received, f"the graph's frames never reached the server: {received}"


def test_an_unsendable_value_raises():
    g = wf.Graph()
    conn = wf.WsConnection(g, "ws://127.0.0.1:1/stream", max_attempts=1)
    with pytest.raises(RuntimeError, match="str"):
        conn.send({"not": "a frame"})
