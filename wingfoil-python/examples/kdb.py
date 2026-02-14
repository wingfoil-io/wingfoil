#!/usr/bin/env python3
"""KDB+ adapter example: reads trades, computes running average price, and prints results.

Setup:
    Start a KDB+ instance on port 5000 and create/populate the trade table:

    $ q -p 5000

    Then in the q console:

    trade:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())
    n:100
    insert[`trade;(2000.01.01D00:00:00.000000000+1000000000*til n;n?`AAPL`GOOG`MSFT;n?100.0;n?1000j)]

Run:
    cd wingfoil-python && maturin develop && python examples/kdb.py
"""

from wingfoil import kdb_read

# Read trades from KDB, extract price, compute running average
(
    kdb_read(
        host="localhost",
        port=5000,
        query="select from trade",
        time_col="time",
        chunk_size=10000,
    )
    .map(lambda row: row["price"])
    .logged("price")
    .average()
    .logged("avg")
    .run(realtime=False)
)
