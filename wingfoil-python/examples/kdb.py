#!/usr/bin/env python3
"""KDB+ adapter example: writes test trades to KDB+ then reads them back.

Start a KDB+ instance on port 5000 and create the table:
    q -p 5000
    test_trades:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())

Run:
    cd wingfoil-python && python examples/kdb.py

Reset the table between runs:
    delete from `test_trades
"""

from wingfoil import ticker, kdb_read

HOST = "localhost"
PORT = 5000
TABLE = "test_trades"
NUM_ROWS = 10


def generate(num_rows):
    syms = ["AAPL", "GOOG", "MSFT"]
    return (
        ticker(1.0)
        .count()
        .map(lambda i: {
            "sym": syms[i % len(syms)],
            "price": 100.0 + float(i),
            "qty": i * 10 + 1,
        })
        .limit(num_rows)
    )


print(f"Writing {NUM_ROWS} rows to {TABLE}...")
generate(NUM_ROWS).kdb_write(
    host=HOST,
    port=PORT,
    table=TABLE,
    columns=[("sym", "symbol"), ("price", "float"), ("qty", "long")],
).run(realtime=False)

print(f"Reading back from {TABLE}...")
rows_stream = kdb_read(
    host=HOST,
    port=PORT,
    query=f"select from {TABLE}",
    time_col="time",
    chunk_size=10000,
).collect()
rows_stream.run(realtime=False)
rows = rows_stream.peek_value()

print(f"Read {len(rows)} rows")
assert len(rows) == NUM_ROWS, f"Expected {NUM_ROWS} rows, got {len(rows)}"
for row in rows:
    assert "sym" in row and "price" in row and "qty" in row
print(f"✓ {NUM_ROWS} rows written and read back successfully")
print("Sample row:", rows[0])
