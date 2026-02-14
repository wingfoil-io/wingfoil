"""Integration tests for KDB+ read/write Python bindings.

Requires a running KDB+ instance on localhost:5000.
Skip these tests if KDB+ is not available by setting KDB_SKIP=1.

Setup:
    q -p 5000
"""

import os
import socket
import unittest

# Skip if KDB+ not available
def kdb_available():
    if os.environ.get("KDB_SKIP", "0") == "1":
        return False
    try:
        s = socket.create_connection(("localhost", 5000), timeout=1)
        s.close()
        return True
    except (ConnectionRefusedError, OSError):
        return False

KDB_AVAILABLE = kdb_available()


@unittest.skipUnless(KDB_AVAILABLE, "KDB+ not running on localhost:5000")
class TestKdbRead(unittest.TestCase):
    """Test kdb_read functionality."""

    def setUp(self):
        """Create and populate test table via kdb_write or raw connection."""
        # We rely on the table being set up externally or by a prior test
        # For CI, the table should be pre-populated
        pass

    def test_kdb_read_returns_dicts(self):
        """kdb_read should return a stream of dicts."""
        from wingfoil import kdb_read

        stream = (
            kdb_read(
                host="localhost",
                port=5000,
                query="select from trade",
                time_col="time",
                chunk_size=10000,
            )
            .collect()
        )
        stream.run(realtime=False, cycles=3)
        rows = stream.peek_value()
        self.assertIsInstance(rows, list)
        if len(rows) > 0:
            row = rows[0]
            self.assertIsInstance(row, dict)
            self.assertIn("sym", row)
            self.assertIn("price", row)
            self.assertIn("qty", row)


@unittest.skipUnless(KDB_AVAILABLE, "KDB+ not running on localhost:5000")
class TestKdbWrite(unittest.TestCase):
    """Test kdb_write round-trip functionality."""

    def test_kdb_write_method(self):
        """Stream.kdb_write() should write dicts to a KDB+ table."""
        from wingfoil import kdb_read, constant

        # Create a dict representing a trade row
        trade = {"sym": "TEST", "price": 42.0, "qty": 100}

        # Write via stream method
        writer = (
            constant(trade)
                .kdb_write(
                    host="localhost",
                    port=5000,
                    table="trade",
                    columns=[("sym", "symbol"), ("price", "float"), ("qty", "long")],
                )
        )
        writer.run(realtime=False, cycles=1)

        # Verify by reading back
        stream = (
            kdb_read(
                host="localhost",
                port=5000,
                query="select from trade where sym=`TEST",
                time_col="time",
                chunk_size=10000,
            )
            .collect()
        )
        stream.run(realtime=False)
        rows = stream.peek_value()
        self.assertGreaterEqual(len(rows), 1)
        found = [r for r in rows if r.get("sym") == "TEST"]
        self.assertGreaterEqual(len(found), 1)
        self.assertAlmostEqual(found[0]["price"], 42.0, places=2)


if __name__ == "__main__":
    unittest.main()
