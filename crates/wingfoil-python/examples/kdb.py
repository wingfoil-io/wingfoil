#!/usr/bin/env python3
"""KDB+ adapter — write rows to a table, then read them back.

Prerequisites — a KDB+ instance on port 5000 with the target table:

    q -p 5000
    q) test_trades:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())

Run:

    cd crates/wingfoil-python && maturin develop --features kdb
    python examples/kdb.py

Reset between runs:

    q) delete from `test_trades

Ported from legacy wingfoil's `examples/kdb.py`. Two shape differences:
`kdb_read` takes the replay window (`start_nanos`, `duration_nanos`) rather
than an open-ended query, and the graph is explicit — the write and the read
are separate graphs because the second must not start until the first is done.
"""

import wingfoil as wf

HOST = "localhost"
PORT = 5000
TABLE = "test_trades"
NUM_ROWS = 10
SYMS = ["AAPL", "GOOG", "MSFT"]

SECOND_NANOS = 1_000_000_000
DAY_NANOS = 86_400 * SECOND_NANOS
# KDB+ timestamps count from 2000-01-01, so a replay window has to start there.
KDB_EPOCH_NANOS = 946_684_800 * SECOND_NANOS

print(f"Writing {NUM_ROWS} rows to {TABLE}...")

g = wf.Graph()
rows = (
    g.counter(period_nanos=SECOND_NANOS)
    .limit(NUM_ROWS)
    .map(
        lambda i: {
            "sym": SYMS[i % len(SYMS)],
            "price": 100.0 + float(i),
            "qty": i * 10 + 1,
        }
    )
)
wf.kdb_write(
    rows,
    HOST,
    PORT,
    TABLE,
    [("sym", "symbol"), ("price", "float"), ("qty", "long")],
)
g.run(cycles=NUM_ROWS, start_nanos=KDB_EPOCH_NANOS)

print(f"Reading back from {TABLE}...")

g = wf.Graph()
collected = wf.kdb_read(
    g,
    HOST,
    PORT,
    f"select sym, price, qty, time from {TABLE}",
    "time",
    KDB_EPOCH_NANOS,
    DAY_NANOS,
).collect()
g.run(duration_nanos=DAY_NANOS, start_nanos=KDB_EPOCH_NANOS)

read = [row for _time, burst in collected.value() for row in burst]
print(f"Read {len(read)} rows")
assert len(read) == NUM_ROWS, f"Expected {NUM_ROWS} rows, got {len(read)}"
for row in read:
    assert "sym" in row and "price" in row and "qty" in row
print(f"✓ {NUM_ROWS} rows written and read back successfully")
print("Sample row:", read[0])
