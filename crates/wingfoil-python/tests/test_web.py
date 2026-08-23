"""Tests for the wingfoil web Python bindings.

Two groups:

- Server lifecycle / marshaling tests (no marker): run by default. A
  ``historical=True`` server binds no port at all, and marshaling happens on the
  graph side of the sink, so the whole value edge is exercisable without a
  socket. The live-server checks that need only HTTP (static files, the bound
  port) use `urllib` from the standard library.
- WebSocket round trips (``@pytest.mark.requires_web``): deselected by default.
  These speak the real wire protocol against the real server, so they need the
  ``websockets`` package — that is the only thing the marker gates; there is no
  external service. The web integration workflow installs it and opts in
  with ``-m requires_web``.
"""

import json
import threading
import time
import urllib.request

import pytest

import wingfoil as wf

MS_NANOS = 1_000_000
SECOND_NANOS = 1_000_000_000

CONTROL_TOPIC = "$ctrl"


def envelope(topic, payload_obj):
    """One wire frame, JSON codec: `payload` is a byte array of the inner JSON."""
    return json.dumps(
        {
            "topic": topic,
            "time_ns": 0,
            "payload": list(json.dumps(payload_obj).encode()),
        }
    ).encode()


def payload_of(frame):
    """The decoded user value out of a server frame (JSON codec)."""
    body = json.loads(frame)
    return body["topic"], json.loads(bytes(body["payload"]))


# ---- Server lifecycle and marshaling (no socket needed) ----


def test_module_exposes_the_server():
    assert isinstance(wf.WebServer, type)


def test_a_live_server_binds_a_port():
    server = wf.WebServer("127.0.0.1:0")
    try:
        assert server.port() > 0
        assert server.codec_name() == "bincode"
        assert not server.is_tls()
        assert not server.is_historical_noop()
    finally:
        server.stop()


def test_the_codec_is_selectable():
    server = wf.WebServer("127.0.0.1:0", codec="json")
    try:
        assert server.codec_name() == "json"
    finally:
        server.stop()


def test_delivery_defaults_to_auto_and_is_selectable():
    server = wf.WebServer("127.0.0.1:0")
    try:
        assert server.delivery_name() == "auto"
    finally:
        server.stop()

    for chosen in ("auto", "lossy", "lossless"):
        server = wf.WebServer("127.0.0.1:0", delivery=chosen)
        try:
            assert server.delivery_name() == chosen
        finally:
            server.stop()


def test_the_lossless_stall_timeout_defaults_and_is_selectable():
    server = wf.WebServer("127.0.0.1:0")
    try:
        assert server.lossless_stall_timeout_secs() == 30.0
    finally:
        server.stop()

    server = wf.WebServer("127.0.0.1:0", lossless_stall_timeout_secs=0.25)
    try:
        assert server.lossless_stall_timeout_secs() == 0.25
    finally:
        server.stop()


def test_a_non_positive_stall_timeout_raises():
    with pytest.raises(RuntimeError) as excinfo:
        wf.WebServer("127.0.0.1:0", lossless_stall_timeout_secs=0.0)
    assert "must be a finite, positive number" in str(excinfo.value)


def test_a_stall_timeout_too_large_for_a_duration_raises():
    with pytest.raises(RuntimeError) as excinfo:
        wf.WebServer("127.0.0.1:0", lossless_stall_timeout_secs=1e20)
    assert "too large" in str(excinfo.value)


def test_an_unknown_delivery_raises():
    with pytest.raises(RuntimeError) as excinfo:
        wf.WebServer("127.0.0.1:0", delivery="best-effort")
    assert "expected 'auto', 'lossy' or 'lossless'" in str(excinfo.value)


def test_an_unknown_codec_raises():
    with pytest.raises(RuntimeError) as excinfo:
        wf.WebServer("127.0.0.1:0", codec="protobuf")
    assert "expected 'bincode' or 'json'" in str(excinfo.value)


def test_half_a_tls_pair_raises():
    with pytest.raises(RuntimeError) as excinfo:
        wf.WebServer("127.0.0.1:0", cert_path="cert.pem")
    assert "must be given together" in str(excinfo.value)


def test_an_unusable_address_raises():
    with pytest.raises(RuntimeError):
        wf.WebServer("not-an-address")


def test_a_historical_server_binds_nothing():
    server = wf.WebServer("127.0.0.1:0", historical=True)
    assert server.is_historical_noop()
    assert server.port() == 0


def test_stop_is_idempotent():
    server = wf.WebServer("127.0.0.1:0")
    server.stop()
    server.stop()


def test_static_files_are_served(tmp_path):
    (tmp_path / "index.html").write_text("<h1>wingfoil</h1>")

    server = wf.WebServer("127.0.0.1:0", static_dir=str(tmp_path))
    try:
        url = f"http://127.0.0.1:{server.port()}/index.html"
        with urllib.request.urlopen(url, timeout=5) as body:
            assert "wingfoil" in body.read().decode()
    finally:
        server.stop()


def _noop_server(codec="json"):
    """A no-op server: no port, no sockets — but the value edge still runs.

    Defaults to the JSON codec because `sub` requires it (see
    `test_sub_rejects_the_bincode_codec`); `pub` is codec-agnostic here, since
    marshaling happens on the graph side of the sink.
    """
    return wf.WebServer("127.0.0.1:0", codec=codec, historical=True)


def test_the_three_wiring_calls_return_streams():
    g = wf.Graph()
    server = _noop_server()
    assert isinstance(server.pub(g.constant(1.0), "out"), wf.Stream)
    assert isinstance(server.pub_bursts(g.constant([1.0, 2.0]), "out_bursts"), wf.Stream)
    assert isinstance(server.sub(g, "in"), wf.Stream)


def _run_pub(value):
    """Publish one tick of `value` on a no-op server, returning the error."""
    g = wf.Graph()
    server = _noop_server()
    server.pub(g.constant(value), "out")
    with pytest.raises(RuntimeError) as excinfo:
        g.run(cycles=1)
    return str(excinfo.value)


def test_an_unsupported_value_type_aborts_the_run():
    """Legacy published these as JSON `null`; this fails loudly instead."""
    assert "unsupported value type" in _run_pub({1, 2})


def test_a_non_str_dict_key_aborts_the_run():
    assert "dict keys must be str" in _run_pub({1: "a"})


def test_a_non_finite_float_aborts_the_run():
    assert "no JSON representation" in _run_pub(float("inf"))


def test_a_historical_sub_never_ticks():
    """Live browser input has no place in a deterministic replay."""
    g = wf.Graph()
    server = _noop_server()
    seen = server.sub(g, "in").accumulate()
    g.run(duration_nanos=SECOND_NANOS)
    # Never ticked, so the accumulator never ticked either — `None`, not `[]`.
    assert seen.value() is None


# ---- The codec matrix (the Python sibling of #821) ----
#
# Payloads marshal through a schema-less `serde_json::Value`. bincode is not
# self-describing, so it cannot *decode* one at all — which makes `sub`
# impossible under bincode for every peer, and makes `pub` peer-dependent
# rather than broken. The Rust unit tests in `src/adapters/web.rs` pin the
# codec bytes behind these; these pin the Python-visible behaviour.


def test_sub_rejects_the_bincode_codec():
    """Every frame would abort the run, so the call that chose it raises."""
    server = wf.WebServer("127.0.0.1:0")  # bincode is the default
    try:
        assert server.codec_name() == "bincode"
        with pytest.raises(RuntimeError) as excinfo:
            server.sub(wf.Graph(), "clicks")
    finally:
        server.stop()
    message = str(excinfo.value)
    assert 'codec="json"' in message
    assert "self-describing" in message


def test_sub_rejects_bincode_on_a_historical_server_too():
    """A backtest that cannot also run live is a misconfiguration, not a pass."""
    server = _noop_server(codec="bincode")
    with pytest.raises(RuntimeError) as excinfo:
        server.sub(wf.Graph(), "clicks")
    assert 'codec="json"' in str(excinfo.value)


def test_sub_accepts_the_json_codec():
    server = wf.WebServer("127.0.0.1:0", codec="json")
    try:
        assert isinstance(server.sub(wf.Graph(), "clicks"), wf.Stream)
    finally:
        server.stop()


def test_pub_still_accepts_the_bincode_codec():
    """Not rejected: a Rust `web_sub::<f64>` / `<String>` / `<Vec<f64>>` peer
    really does read a bincode publish, and the adapter cannot see the peer's
    type. Only the docs can carry that caveat."""
    g = wf.Graph()
    server = _noop_server(codec="bincode")
    assert isinstance(server.pub(g.constant(1.5), "px"), wf.Stream)
    assert isinstance(server.pub_bursts(g.constant([1.0, 2.0]), "book"), wf.Stream)
    g.run(cycles=1)


# ---- WebSocket round trips (need the `websockets` package) ----


def _client(port, topics, collected, ready, stop, to_send=()):
    """Subscribe to `topics`, append every frame to `collected` until `stop`.

    Runs on its own thread while the graph runs on the main one — `Graph.run`
    releases the GIL, so both make progress.
    """
    from websockets.sync.client import connect

    with connect(f"ws://127.0.0.1:{port}/ws", open_timeout=5) as socket:
        socket.recv()  # the server's Hello, on the control topic
        socket.send(envelope(CONTROL_TOPIC, {"Subscribe": {"topics": list(topics)}}))
        # The server applies the subscription asynchronously; give it a moment
        # before telling the main thread it may start publishing.
        time.sleep(0.2)
        ready.set()

        for topic, value in to_send:
            socket.send(envelope(topic, value))
            time.sleep(0.05)

        while not stop.is_set():
            try:
                collected.append(socket.recv(timeout=0.1))
            except TimeoutError:
                continue


def _round_trip(port, topics, run, to_send=()):
    """Run `run()` with a subscribed client attached; return its frames."""
    collected, ready, stop = [], threading.Event(), threading.Event()
    thread = threading.Thread(
        target=_client, args=(port, topics, collected, ready, stop, to_send)
    )
    thread.start()
    try:
        assert ready.wait(timeout=10), "client never subscribed"
        run()
        # Let the last frames land before closing the socket.
        time.sleep(0.3)
    finally:
        stop.set()
        thread.join(timeout=10)
    return collected


@pytest.mark.requires_web
def test_pub_reaches_a_subscribed_client():
    g = wf.Graph()
    server = wf.WebServer("127.0.0.1:0", codec="json")
    try:
        server.pub(g.counter(period_nanos=50 * MS_NANOS), "ticks")
        frames = _round_trip(
            server.port(),
            ["ticks"],
            lambda: g.run(realtime=True, duration_nanos=500 * MS_NANOS),
        )
    finally:
        server.stop()

    values = [payload for topic, payload in map(payload_of, frames) if topic == "ticks"]
    assert values, "no frames arrived on 'ticks'"
    # `counter` counts up, one value per frame.
    assert values == sorted(values)


@pytest.mark.requires_web
def test_pub_marshals_the_python_shapes():
    g = wf.Graph()
    server = wf.WebServer("127.0.0.1:0", codec="json")
    record = {"sym": "AAPL", "px": 1.5, "live": True, "tags": [1, 2], "note": None}
    try:
        server.pub(g.constant(record), "book")
        frames = _round_trip(
            server.port(),
            ["book"],
            lambda: g.run(realtime=True, duration_nanos=300 * MS_NANOS),
        )
    finally:
        server.stop()

    values = [payload for topic, payload in map(payload_of, frames) if topic == "book"]
    assert values and values[0] == record


@pytest.mark.requires_web
def test_pub_bursts_puts_a_whole_group_in_one_frame():
    """A list on one tick crosses as a single array frame, never split."""
    g = wf.Graph()
    server = wf.WebServer("127.0.0.1:0", codec="json")
    try:
        server.pub_bursts(g.constant([{"n": 1}, {"n": 2}]), "group")
        frames = _round_trip(
            server.port(),
            ["group"],
            lambda: g.run(realtime=True, duration_nanos=300 * MS_NANOS),
        )
    finally:
        server.stop()

    values = [payload for topic, payload in map(payload_of, frames) if topic == "group"]
    assert values and values[0] == [{"n": 1}, {"n": 2}]


@pytest.mark.requires_web
def test_sub_decodes_frames_a_client_sends():
    g = wf.Graph()
    server = wf.WebServer("127.0.0.1:0", codec="json")
    try:
        seen = server.sub(g, "clicks").accumulate()
        _round_trip(
            server.port(),
            ["clicks"],
            lambda: g.run(realtime=True, duration_nanos=800 * MS_NANOS),
            to_send=[("clicks", {"x": 1}), ("clicks", {"x": 2})],
        )
    finally:
        server.stop()

    # Each tick is a list of the frames that arrived between cycles.
    received = [value for tick in seen.value() for value in tick]
    assert received == [{"x": 1}, {"x": 2}]
