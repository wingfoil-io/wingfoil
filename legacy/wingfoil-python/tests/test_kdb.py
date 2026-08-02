"""Integration tests for KDB+ read/write Python bindings.

Selected via `-m requires_kdb`. Without a KDB+ instance on localhost:5000
the tests will fail loudly — they do not silently skip.

Setup:
    q -p 5000
"""

import socket
import struct
import unittest

import pytest

TABLE = "py_kdb_test_trades"
HOST = "localhost"
PORT = 5000


def q_exec(query: str):
    """Send a synchronous q expression to the running q process.

    The q IPC binary format:
      - Handshake: send capability string (empty user, version 3), read 1-byte response
      - Message header: 8 bytes [endian=1, type=1(sync), 0, 0, total_len(4 bytes LE)]
      - Body: char vector type 10, attr 0, length (4 bytes LE), UTF-8 bytes
      - q evaluates the char vector via the default .z.pg handler (value x)

    Raises RuntimeError if q returns a type-128 error.
    """
    query_bytes = query.encode("ascii")
    body = bytes([10, 0]) + struct.pack("<I", len(query_bytes)) + query_bytes
    total_len = 8 + len(body)
    msg = bytes([1, 1, 0, 0]) + struct.pack("<I", total_len) + body

    s = socket.create_connection((HOST, PORT), timeout=5)
    try:
        # Handshake: empty credentials, capability byte 3, null terminator
        s.sendall(b"\x03\x00")
        s.recv(1)  # capability byte echo

        s.sendall(msg)

        # Read response header (8 bytes)
        resp_header = _recv_exact(s, 8)
        resp_total = struct.unpack("<I", resp_header[4:8])[0]
        resp_body = _recv_exact(s, resp_total - 8)

        # Type byte 0x80 (-128 signed) indicates a q error
        if resp_body and resp_body[0] == 0x80:
            err = resp_body[2:].rstrip(b"\x00").decode("ascii", errors="replace")
            raise RuntimeError(f"q error: {err}")
    finally:
        s.close()


def _recv_exact(s: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            raise EOFError(f"connection closed after {len(buf)}/{n} bytes")
        buf += chunk
    return buf


@pytest.mark.requires_kdb
class TestKdbRead(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        q_exec(f"{TABLE}:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())")
        q_exec(f"`{TABLE} insert (2000.01.01D00:00:01.000000000;`AAPL;100.0;10)")
        q_exec(f"`{TABLE} insert (2000.01.01D00:00:02.000000000;`GOOG;200.0;20)")
        q_exec(f"`{TABLE} insert (2000.01.01D00:00:03.000000000;`MSFT;300.0;30)")

    @classmethod
    def tearDownClass(cls):
        q_exec(f"delete {TABLE} from `.")

    def test_kdb_read_returns_dicts(self):
        """kdb_read returns a stream of dicts with expected keys and values."""
        from wingfoil import kdb_read

        stream = kdb_read(
            host=HOST,
            port=PORT,
            query=f"select from {TABLE}",
            time_col="time",
            chunk_size=10000,
        ).collect()
        # start=2000-01-01 00:00:00 UTC (Unix seconds), duration=1 day
        stream.run(realtime=False, start=946684800.0, duration=86400.0)
        rows = stream.peek_value()

        self.assertIsInstance(rows, list)
        self.assertEqual(len(rows), 3)
        row = rows[0]
        self.assertIsInstance(row, dict)
        self.assertIn("sym", row)
        self.assertIn("price", row)
        self.assertIn("qty", row)
        syms = {r["sym"] for r in rows}
        self.assertEqual(syms, {"AAPL", "GOOG", "MSFT"})


@pytest.mark.requires_kdb
class TestKdbWrite(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        q_exec(
            f"{TABLE}_write:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())"
        )

    @classmethod
    def tearDownClass(cls):
        q_exec(f"delete {TABLE}_write from `.")

    def test_kdb_write_round_trip(self):
        """kdb_write inserts rows that kdb_read can read back."""
        from wingfoil import kdb_read, constant

        write_table = f"{TABLE}_write"

        trade = {"sym": "TEST", "price": 42.0, "qty": 100}
        constant(trade).kdb_write(
            host=HOST,
            port=PORT,
            table=write_table,
            columns=[("sym", "symbol"), ("price", "float"), ("qty", "long")],
        ).run(realtime=False, start=946684800.0, cycles=1)

        stream = kdb_read(
            host=HOST,
            port=PORT,
            query=f"select from {write_table}",
            time_col="time",
            chunk_size=10000,
        ).collect()
        # start=2000-01-01 00:00:00 UTC (Unix seconds), duration=1 day
        stream.run(realtime=False, start=946684800.0, duration=86400.0)
        rows = stream.peek_value()

        self.assertGreaterEqual(len(rows), 1)
        found = [r for r in rows if r.get("sym") == "TEST"]
        self.assertGreaterEqual(len(found), 1)
        self.assertAlmostEqual(found[0]["price"], 42.0, places=2)
        self.assertEqual(found[0]["qty"], 100)


if __name__ == "__main__":
    unittest.main()
