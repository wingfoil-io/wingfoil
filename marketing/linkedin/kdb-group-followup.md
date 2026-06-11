# LinkedIn — KDB+ adapter (follow-up for the KDB group)

## Post

Following up on last week's Wingfoil v3 announcement, but writing this one with the KDB crowd specifically in mind — "we integrate with KDB" tends to mean very different things depending on the library, so a bit more detail on the design choices.

**You write the q, not us.** `kdb_read` takes a closure that returns the query string for each time slice. We don't try to abstract over splays, partition columns, or where-clause ordering — you know your tables better than any wrapper will. The adapter handles the slicing, the cursor, and the row-to-struct mapping. The query stays yours.

**Time slicing is driven by the graph's run mode.** Set `RunMode::HistoricalFrom(start)` and `RunFor::Duration(d)`, and the adapter computes half-open `[t0, t1)` slices and calls your closure for each one. A day of trades streams through your graph one chunk at a time instead of materialising in memory. Boundary convention uses ``(`timestamp$){}j`` against your time column, so slices line up cleanly with whatever partitioning you've got.

**`Sym` is a first-class type.** Symbols are interned on read and round-trip back as KDB symbols on write. Time lives on the graph as `(NanoTime, T)` tuples, so your business struct stays clean — no time field, no double-counting.

There's also `kdb_read_cached`, which is the piece that's saved us the most wall-clock time in practice. Same closure-driven API, plus a cache directory; each slice is bincode'd to disk on first read and served from there afterwards. The TCP connection is lazy, so a fully cached backtest doesn't open a connection to KDB at all. Useful when you're iterating on a strategy and re-running the same window twenty times an afternoon.

Writes go through a functional `(insert; `tablename; row_values)`, so the schema and on-disk layout stay defined by your `.q` setup.

Available in Rust and via the PyO3 Python bindings.

## First comment

- Adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/kdb
- Round-trip example: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kdb/round_trip
- Time-sliced read example: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kdb/read
- Cached read example: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/examples/kdb/read_cached
- Wingfoil repo: https://github.com/wingfoil-io/wingfoil
