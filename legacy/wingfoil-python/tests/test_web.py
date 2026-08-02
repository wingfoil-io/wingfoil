"""Integration tests for the wingfoil web adapter Python bindings.

Selected via ``pytest -m requires_web``. The tests spin up an in-process
``WebServer`` (no Docker required) and exercise the Rust adapter through
Python APIs.
"""

import json
import struct
import threading
import time
import unittest
from socket import socket
from urllib.request import Request, urlopen

import pytest


def _ws_handshake(sock: socket, port: int) -> None:
    """Minimal RFC 6455 client handshake; returns once the upgrade succeeds."""
    import base64
    import os

    key = base64.b64encode(os.urandom(16)).decode()
    req = (
        f"GET /ws HTTP/1.1\r\n"
        f"Host: 127.0.0.1:{port}\r\n"
        f"Upgrade: websocket\r\n"
        f"Connection: Upgrade\r\n"
        f"Sec-WebSocket-Key: {key}\r\n"
        f"Sec-WebSocket-Version: 13\r\n\r\n"
    )
    sock.sendall(req.encode())
    # Drain headers up to the blank line.
    buf = b""
    while b"\r\n\r\n" not in buf:
        chunk = sock.recv(1024)
        if not chunk:
            raise RuntimeError("ws handshake closed early")
        buf += chunk


def _ws_send_frame(sock: socket, payload: bytes) -> None:
    """Send a single masked binary frame (client → server must be masked)."""
    import os

    header = bytearray([0x82])  # FIN + binary
    length = len(payload)
    mask_key = os.urandom(4)
    if length < 126:
        header.append(0x80 | length)
    elif length < (1 << 16):
        header.append(0x80 | 126)
        header.extend(struct.pack(">H", length))
    else:
        header.append(0x80 | 127)
        header.extend(struct.pack(">Q", length))
    header.extend(mask_key)
    masked = bytes(b ^ mask_key[i % 4] for i, b in enumerate(payload))
    sock.sendall(header + masked)


def _ws_recv_frame(sock: socket) -> bytes:
    """Receive a single frame, returning its payload bytes."""
    header = sock.recv(2)
    if len(header) < 2:
        return b""
    b0, b1 = header
    masked = b1 & 0x80
    length = b1 & 0x7F
    if length == 126:
        length = struct.unpack(">H", sock.recv(2))[0]
    elif length == 127:
        length = struct.unpack(">Q", sock.recv(8))[0]
    mask_key = sock.recv(4) if masked else b""
    payload = b""
    while len(payload) < length:
        payload += sock.recv(length - len(payload))
    if masked:
        payload = bytes(b ^ mask_key[i % 4] for i, b in enumerate(payload))
    return payload


@pytest.mark.requires_web
class TestWebBindings(unittest.TestCase):
    def test_bind_and_pub_roundtrip_json(self):
        from wingfoil import WebServer, ticker

        server = WebServer("127.0.0.1:0", codec="json")
        port = server.port()
        self.assertGreater(port, 0)
        self.assertEqual(server.codec_name(), "json")

        # Client in a background thread: connect, subscribe, capture frames.
        received: list[dict] = []
        ready = threading.Event()

        def run_client():
            import socket as s

            with s.create_connection(("127.0.0.1", port), timeout=5) as sock:
                _ws_handshake(sock, port)
                # Drain the server Hello control frame.
                _ws_recv_frame(sock)
                # Send a Subscribe control frame (JSON codec).
                envelope = {
                    "topic": "$ctrl",
                    "time_ns": 0,
                    "payload": list(
                        json.dumps({"Subscribe": {"topics": ["tick"]}}).encode()
                    ),
                }
                _ws_send_frame(sock, json.dumps(envelope).encode())
                ready.set()
                # Receive a handful of frames.
                for _ in range(3):
                    raw = _ws_recv_frame(sock)
                    if not raw:
                        break
                    env = json.loads(raw)
                    if env["topic"] == "tick":
                        payload = bytes(env["payload"]).decode()
                        received.append(json.loads(payload))

        client = threading.Thread(target=run_client)
        client.start()
        self.assertTrue(ready.wait(timeout=5), "client never subscribed")

        stream = ticker(0.02).count().web_pub(server, "tick")
        stream.run(realtime=True, duration=0.4)
        client.join(timeout=10)

        self.assertGreater(len(received), 0, "no frames received over WS")

    def test_historical_mode_is_noop(self):
        from wingfoil import WebServer, constant

        server = WebServer("127.0.0.1:0", historical=True)
        self.assertEqual(server.port(), 0)

        node = constant({"x": 1}).web_pub(server, "topic")
        # Should just terminate cleanly — no TCP bind, no client.
        node.run(realtime=False, cycles=1)


if __name__ == "__main__":
    unittest.main()
