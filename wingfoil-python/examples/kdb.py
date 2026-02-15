#!/usr/bin/env python3
"""KDB+ adapter example: generates test trades, writes to KDB, reads back, and validates.

This example demonstrates the full write/read cycle similar to the Rust version:
1. Generate test trade data (sym, price, qty)
2. Write to KDB+ test_trades table
3. Read back from KDB+
4. Validate that written data matches read data

Setup:
    Start a KDB+ instance on port 5000:

    $ q -p 5000

Run:
    cd wingfoil-python && maturin develop && python examples/kdb.py

Note:
    If you re-run without manually deleting the data from the first run,
    the validation may fail due to duplicate data.
    To clear: in q console run: delete from `test_trades
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


# Generate test data
baseline = generate(NUM_ROWS)

# Write to KDB
print(f"Writing {NUM_ROWS} rows to {TABLE}...")
(
    baseline
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

# Read back from KDB
print(f"Reading from {TABLE}...")
read_data = kdb_read(
    host=HOST,
    port=PORT,
    query=f"`{TIME_COL} xasc select from {TABLE}",  # Sort by time
    time_col=TIME_COL,
    chunk_size=CHUNK_SIZE,
)

# Validate: generate fresh baseline and compare with read data
print("Validating...")
baseline_fresh = generate(NUM_ROWS)

# Check that data matches
def assert_equal(a, b):
    """Assert two values are equal, with helpful error message."""
    if a != b:
        raise AssertionError(
            f"Generated and read data did not match: {a} != {b}. "
            "This will happen if you re-run without manually deleting "
            "the data from the first run."
        )

# Compare the actual data
# Note: We can only run the validation once because async producers
# can only be set up once. To validate both data and counts together,
# we would need to use Graph.new() which isn't exposed to Python yet.
bimap(baseline_fresh, read_data, assert_equal).run(realtime=False)

print(f"✓ {NUM_ROWS} written and read back successfully")
