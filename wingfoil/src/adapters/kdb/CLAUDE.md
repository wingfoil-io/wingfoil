# KDB+ Adapter

This directory contains the KDB+ adapter for wingfoil, enabling reading from and writing to KDB+ databases.

## Module Structure

```
kdb/
  mod.rs               # Public API, connection types, error handling
  read.rs              # Read functions: kdb_read
  write.rs             # kdb_write() - writing stream data to KDB+ tables
  integration_tests.rs # Integration tests (requires running KDB+ instance)
```

## Key Components

### Reading from KDB+

- `kdb_read()` - Time-partitioned reads driven by a caller-supplied query closure
  - Computes time slices from `RunMode`/`RunFor` and calls `query_fn` for each slice
  - `query_fn((t0, t1), kdb_date, iteration) -> String` — half-open [t0, t1)
  - Use `time >= t0j, time < t1j` in the q filter for clean round-number boundaries
  - Requires `RunMode::HistoricalFrom` (non-zero start) and `RunFor::Duration`
  - Caller constructs the full query — date/time filters, partition hints, etc.
  - Terminates automatically when all slices are exhausted
- `KdbDeserialize` trait - Convert KDB+ rows to Rust types
  - **IMPORTANT**: Do NOT extract the time column - it's handled automatically
  - Your struct should only contain business data (sym, price, qty, etc.)

### Writing to KDB+

- `kdb_write()` - Write stream data to KDB+ tables
  - Time is automatically extracted from graph tuples `(NanoTime, T)` and prepended to rows
- `KdbSerialize` trait - Convert Rust types to KDB+ rows
  - `to_kdb_row()` - Returns K object with business data only (no time)
  - **IMPORTANT**: Do NOT include the time field - it's prepended automatically
  - Your struct should only contain business data (sym, price, qty, etc.)

### Connection

- `KdbConnection::new(host, port)` - Configure connection
- `.with_credentials(user, pass)` - Add authentication

## Pre-Commit Requirements

Before committing changes to the KDB adapter, you MUST:

1. **Run integration tests with a live KDB+ instance:**

   ```bash
   # Start KDB+ on port 5000
   # In a separate terminal:
   q -p 5000

   # Then run integration tests:
   cargo test --features kdb-integration-test -p wingfoil
   ```

2. **Run standard formatting and linting:**

   ```bash
   cargo fmt --all
   cargo clippy --workspace --all-targets --all-features
   ```

3. **Run unit tests:**

   ```bash
   cargo test -p wingfoil
   ```

## Integration Test Details

The integration tests in `integration_tests.rs` are gated behind the `kdb-integration-test` feature flag to avoid requiring a running KDB+ instance for normal development.

**Environment variables:**
- `KDB_TEST_HOST` - KDB+ host (default: "localhost")
- `KDB_TEST_PORT` - KDB+ port (default: 5000)

**Test coverage:**
- Round-trip write/read verification
- Serialization/deserialization correctness
- Connection handling
- Time-sliced reads across multiple days and periods
- Edge cases: empty tables, bad queries, bad time columns

## Development Tips

- Always implement both `KdbSerialize` and `KdbDeserialize` for types used in round-trip scenarios
- **Time management**: Time is stored on-graph in tuples `(NanoTime, T)`, not in record structs
  - When reading: time is extracted from the KDB time column and passed as the first tuple element
  - When writing: time is extracted from the tuple and prepended to the serialized row
  - Your structs should ONLY contain business data (no time field)
- Write operations use K object functional queries: `(insert; `tablename; row_values)`
- Connection pooling: Each read/write call opens its own connection

### Example: Record Structure

```rust
// CORRECT - No time field, use Sym for interned symbols
#[derive(Debug, Clone, Default)]
struct Trade {
    sym: Sym,
    price: f64,
    qty: i64,
}

impl KdbDeserialize for Trade {
    fn from_kdb_row(row: Row<'_>, _columns: &[String], interner: &mut SymbolInterner) -> Result<Self, KdbError> {
        Ok(Trade {
            // col 0: date (skip), col 1: time (handled by adapter)
            sym: row.get_sym(2, interner)?,
            price: row.get(3)?.get_float()?,
            qty: row.get(4)?.get_long()?,
        })
    }
}

impl KdbSerialize for Trade {
    fn to_kdb_row(&self) -> K {
        // Return business data only - time prepended automatically
        K::new_compound_list(vec![
            K::new_symbol(self.sym.to_string()),
            K::new_float(self.price),
            K::new_long(self.qty),
        ])
    }
}
```

### Reading with kdb_read

```rust
// Date-partitioned table: filter by date and time range in each slice
let stream = kdb_read::<Trade, _>(
    conn,
    std::time::Duration::from_secs(3600), // 1-hour slices
    |(t0, t1), date, _| {
        format!(
            "select from trades where date=2000.01.01+{}, \
             time >= (`timestamp$){}j, time < (`timestamp$){}j",
            date, t0.to_kdb_timestamp(), t1.to_kdb_timestamp()
        )
    },
    "time",
);
stream
    .collapse()
    .run(
        RunMode::HistoricalFrom(NanoTime::from_kdb_timestamp(start_kdb)),
        RunFor::Duration(std::time::Duration::from_secs(86400)),
    )
    .unwrap();
```
