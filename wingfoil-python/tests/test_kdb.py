"""Integration tests for KDB+ read/write Python bindings.

Requires a running KDB+ instance on localhost:5000.
Skip these tests if KDB+ is not available by setting KDB_SKIP=1.

Setup:
    q -p 5000
"""

import os
import socket
import struct
import unittest

TABLE = "py_kdb_test_trades"
HOST = "localhost"
PORT = 5000


def kdb_available():
    if os.environ.get("KDB_SKIP", "0") == "1":
        return False
    try:
        s = socket.create_connection((HOST, PORT), timeout=1)
        s.close()
        return True
    except (ConnectionRefusedError, OSError):
        return False


KDB_AVAILABLE = kdb_available()


def q_exec(query: str):
    """Send a synchronous q expression to the running q process.

    Raises RuntimeError if q returns an error.
    """
    query_bytes = query.encode("ascii")
    n = len(query_bytes)

    # Serialize as char vector (type 10)
    body = bytes([10, 0]) + struct.pack("<I", n) + query_bytes

    # 8-byte message header: little-endian, sync request
    total_len = 8 + len(body)
    header = bytes([1, 1, 0, 0]) + struct.pack("<I", total_len)
    msg = header + body

    s = socket.create_connection((HOST, PORT), timeout=5)
    try:
        # Handshake: send anonymous credentials + capability byte 3
        s.sendall(b"\x00\x03\x00")
        s.recv(1)  # server responds with accepted capability byte

        # Send query
        s.sendall(msg)

        # Read response header
        resp_header = _recv_all(s, 8)
        resp_total = struct.unpack("<I", resp_header[4:8])[0]
        resp_body = _recv_all(s, resp_total - 8)

        # Type -128 (0x80) means q error
        if resp_body and resp_body[0] == 0x80:
            err_str = resp_body[2:].decode("ascii", errors="replace").rstrip("\x00")
            raise RuntimeError(f"q error: {err_str}")
    finally:
        s.close()


def _recv_all(s: socket.socket, n: int) -> bytes:
    buf = b""
    while len(buf) < n:
        chunk = s.recv(n - len(buf))
        if not chunk:
            break
        buf += chunk
    return buf


@unittest.skipUnless(KDB_AVAILABLE, "KDB+ not running on localhost:5000")
class TestKdbRead(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        q_exec(f"{TABLE}:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())")
        # Insert a few rows with explicit timestamps (kdb+ 2000.01.01 epoch)
        # KDB+ timestamp: nanoseconds from 2000.01.01
        # Use small positive offsets: 1s, 2s, 3s
        q_exec(
            f"`{TABLE} insert "
            f"(2000.01.01D00:00:01.000000000; `AAPL; 100.0; 10)"
        )
        q_exec(
            f"`{TABLE} insert "
            f"(2000.01.01D00:00:02.000000000; `GOOG; 200.0; 20)"
        )
        q_exec(
            f"`{TABLE} insert "
            f"(2000.01.01D00:00:03.000000000; `MSFT; 300.0; 30)"
        )

    @classmethod
    def tearDownClass(cls):
        q_exec(f"delete {TABLE} from `.")

    def test_kdb_read_returns_dicts(self):
        """kdb_read should return a stream of dicts with expected keys and values."""
        from wingfoil import kdb_read

        stream = kdb_read(
            host=HOST,
            port=PORT,
            query=f"select from {TABLE}",
            time_col="time",
            chunk_size=10000,
        ).collect()
        stream.run(realtime=False)
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


@unittest.skipUnless(KDB_AVAILABLE, "KDB+ not running on localhost:5000")
class TestKdbWrite(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        q_exec(f"{TABLE}_write:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())")

    @classmethod
    def tearDownClass(cls):
        q_exec(f"delete {TABLE}_write from `.")

    def test_kdb_write_round_trip(self):
        """kdb_write should insert rows that kdb_read can read back."""
        from wingfoil import kdb_read, constant

        write_table = f"{TABLE}_write"

        trade = {"sym": "TEST", "price": 42.0, "qty": 100}
        writer = constant(trade).kdb_write(
            host=HOST,
            port=PORT,
            table=write_table,
            columns=[("sym", "symbol"), ("price", "float"), ("qty", "long")],
        )
        writer.run(realtime=False, cycles=1)

        stream = kdb_read(
            host=HOST,
            port=PORT,
            query=f"select from {write_table}",
            time_col="time",
            chunk_size=10000,
        ).collect()
        stream.run(realtime=False)
        rows = stream.peek_value()
        self.assertGreaterEqual(len(rows), 1)
        found = [r for r in rows if r.get("sym") == "TEST"]
        self.assertGreaterEqual(len(found), 1)
        self.assertAlmostEqual(found[0]["price"], 42.0, places=2)
        self.assertEqual(found[0]["qty"], 100)


if __name__ == "__main__":
    unittest.main()
