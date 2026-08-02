# KDB+ Round-Trip Example (wingfoil-next)

Demonstrates the KDB+ adapter end to end by:

1. generating mock trade data,
2. writing it to KDB+ (`kdb_write`),
3. reading it back (`kdb_read`), and
4. validating that the read data matches the generated data.

This is the wingfoil-next port of the legacy `kdb` round-trip example.

## Setup

Start a KDB+ instance on port 5000:

```sh
q -p 5000
```

Then create the `test_trades` table:

```q
test_trades:([]time:`timestamp$();sym:`symbol$();price:`float$();qty:`long$())
```

## Run

```sh
cargo run -p wingfoil-next --example kdb_round_trip --features kdb
```

To reset between runs:

```q
delete from `test_trades
```

## Code

The write side replays the generated trades at their timestamps into a
`Stream<Burst<Trade>>` (via `replay_results`) and sinks them with the
`KdbSinkOps::kdb_write` extension trait; the read side is a time-sliced
`kdb_read`. Both run under `RunMode::HistoricalFrom` for determinism:

```rust
// Write
let rows = baseline.iter().cloned().map(|(time, trade)| Ok((trade, time)));
let _sink = g.replay_results(rows).kdb_write(conn.clone(), TABLE, None)?;
// ... runner.run(...)

// Read + tie-out
let read = kdb_read::<Trade>(&g, params, conn, Duration::from_secs(86400), query_fn, None)?
    .collapse()
    .accumulate();
// ... runner.run(...); assert_eq!(runner.value(&read), expected);
```

## Output

```
✓ 10 written, read and validated
```
