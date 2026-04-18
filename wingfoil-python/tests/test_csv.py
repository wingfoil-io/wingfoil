"""Tests for the CSV Python bindings (py_csv_read and stream.csv_write)."""

import os
import tempfile
import unittest

from wingfoil import Graph, constant, csv_read, ticker


def _write_csv(path: str, text: str) -> None:
    with open(path, "w") as f:
        f.write(text)


class TestCsvRead(unittest.TestCase):
    def test_read_basic_rows(self):
        # IteratorStream needs a trailing row to flush the previous timestamp,
        # so add a sentinel row and filter it out afterwards.
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "data.csv")
            _write_csv(
                path,
                "time,symbol,price\n"
                "1000,AAPL,100\n"
                "2000,MSFT,200\n"
                "3000,GOOG,300\n"
                "9999,SENTINEL,0\n",
            )
            stream = csv_read(path, "time").collect()
            stream.run(realtime=False, cycles=20)
            rows = [r for r in stream.peek_value() if r["symbol"] != "SENTINEL"]
            self.assertEqual(len(rows), 3)
            symbols = [r["symbol"] for r in rows]
            self.assertEqual(symbols, ["AAPL", "MSFT", "GOOG"])
            # Values are preserved as strings
            self.assertEqual(rows[0]["price"], "100")

    def test_read_then_chain_operators(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "data.csv")
            _write_csv(
                path,
                "time,value\n"
                "1000,a\n"
                "2000,b\n"
                "3000,c\n"
                "9999,sentinel\n",
            )
            stream = (
                csv_read(path, "time")
                .map(lambda row: row["value"].upper())
                .collect()
            )
            stream.run(realtime=False, cycles=20)
            uppercased = [v for v in stream.peek_value() if v != "SENTINEL"]
            self.assertEqual(uppercased, ["A", "B", "C"])


class TestCsvWrite(unittest.TestCase):
    def test_csv_write_dict_stream(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_path = os.path.join(tmp, "out.csv")
            stream = (
                ticker(0.1)
                .count()
                .map(lambda n: {"x": n, "y": n * 2})
            )
            node = stream.csv_write(out_path)
            node.run(realtime=False, cycles=3)
            with open(out_path) as f:
                content = f.read()
            lines = [line for line in content.splitlines() if line]
            # First line is header: "time,<dict keys...>"
            self.assertTrue(lines[0].startswith("time,"))
            header_cols = lines[0].split(",")
            self.assertIn("x", header_cols)
            self.assertIn("y", header_cols)
            # Three data rows
            self.assertEqual(len(lines) - 1, 3)

    def test_csv_write_escapes_special_chars(self):
        with tempfile.TemporaryDirectory() as tmp:
            out_path = os.path.join(tmp, "out.csv")
            stream = ticker(0.1).count().map(
                lambda n: {"note": f'has, "quote", and\nnewline #{n}'}
            )
            node = stream.csv_write(out_path)
            node.run(realtime=False, cycles=1)
            with open(out_path) as f:
                content = f.read()
            # Escaped quote should appear as "" inside a quoted field
            self.assertIn('""quote""', content)

    def test_csv_write_roundtrip(self):
        # Round-trip: write via csv_write, read back via csv_read. csv_read's
        # IteratorStream needs a trailing row to flush, so write 3 rows and
        # expect to read back 2 (the last row stays pending).
        with tempfile.TemporaryDirectory() as tmp:
            out_path = os.path.join(tmp, "out.csv")
            stream = (
                ticker(0.1)
                .count()
                .map(lambda n: {"label": f"row{n}", "value": str(n)})
            )
            node = stream.csv_write(out_path)
            node.run(realtime=False, cycles=3)
            read_stream = csv_read(out_path, "time").collect()
            read_stream.run(realtime=False, cycles=20)
            rows = read_stream.peek_value()
            labels = [r["label"] for r in rows]
            self.assertIn("row1", labels)
            self.assertIn("row2", labels)


if __name__ == "__main__":
    unittest.main()
