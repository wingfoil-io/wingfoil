#!/usr/bin/env python3
"""KDB+ adapter example: generates test trades, writes to KDB, reads back, and validates.

This example demonstrates the full write/read cycle similar to the Rust version:
1. Generate test trade data (sym, price, qty)
2. Write to KDB+ test_trades table
3. Read back from KDB+
4. Validate that written data matches read data

Start a KDB+ instance on port 5000 and create the table:
        q -p 5000
        test_trades:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())

Run the example:
    cd wingfoil-python && maturin develop && python examples/kdb.py

Delete from the table:
    delete from `test_trades

Note:
    If you re-run without manually deleting the data from the first run,
    the validation may fail due to duplicate data.
"""

from wingfoil import ticker, kdb_read, bimap

# Configuration
HOST = "localhost"
PORT = 5000
TABLE = "test_trades"
TIME_COL = "time"
CHUNK_SIZE = 10000
NUM_ROWS = 10


def generate(num_rows):
    """Generate test trade data similar to the Rust example."""
    syms = ["AAPL", "GOOG", "MSFT"]

    return (
        ticker(1.0)  # Tick every second (historical mode ignores this)
        .count()
        .map(lambda i: {
            "sym": syms[i % len(syms)],
            "price": 100.0 + float(i),
            "qty": i * 10 + 1,
        })
        .limit(num_rows)
    )

baseline = generate(NUM_ROWS)

print(f"Writing {NUM_ROWS} rows to {TABLE}...")
(
    generate(NUM_ROWS)
        .kdb_write(
            host=HOST,
            port=PORT,
            table=TABLE,
            columns=[
                ("sym", "symbol"),
                ("price", "float"),
                ("qty", "long"),
            ],
        )
        .run(realtime=False)
)

print(f"Reading from {TABLE}...")
read_data = kdb_read(
    host=HOST,
    port=PORT,
    query=f"`{TIME_COL} xasc select from {TABLE}",  # Sort by time
    time_col=TIME_COL,
    chunk_size=CHUNK_SIZE,
)

baseline = generate(NUM_ROWS)
bimap(
    baseline, 
    read_data, 
    lambda a, b: raise AssertionError("Failed to tie out")
)
.run(realtime=False)

print(f"✓ {NUM_ROWS} written and read back successfully")

# DataFrame integration
from wingfoil import stream_to_dataframe,

print("\nConverting read data to DataFrame...")
df = stream_to_dataframe(read_data)
print(df.head())
print(f"DataFrame Shape: {df.shape}")

