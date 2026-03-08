# KDB+ Adapter

This directory contains the KDB+ adapter for wingfoil, enabling reading from and writing to KDB+ databases.

## Module Structure

```
kdb/
  mod.rs               # Public API, connection types, error handling
  read.rs              # kdb_read() - streaming query results into wingfoil
  write.rs             # kdb_write() - writing stream data to KDB+ tables
  integration_tests.rs # Integration tests (requires running KDB+ instance)
```

## Key Components

### Reading from KDB+

- `kdb_read_chunks()` - Primitive chunking function driven by a caller-supplied closure
  - `FnMut(Option<usize>) -> Option<String>`: called before each chunk with last row count
  - `None` on first call, `Some(n)` after each chunk; return `None` to stop
  - Caller owns all query logic: offset arithmetic, date advancement, termination
  - Use for splayed tables where you advance through date partitions manually
- `kdb_read()` - Convenience wrapper over `kdb_read_chunks` with offset-based chunking
  - Uses `(offset;N) sublist query` pagination to handle arbitrarily large result sets with bounded memory
  - Parameters:
    - `connection` - KDB connection configuration
    - `query` - Base q query (can include WHERE clauses)
    - `time_col` - Name of the timestamp column; extracted per-row for stream ordering
    - `date_col` - `Option<&str>`: name of the date partition column, or `None` for in-memory tables.
      When `Some("date")`, injects `date within (start;end)` (bounded) or `date >= start` (unbounded)
      into the query so KDB+ can prune irrelevant date partitions on disk.
    - `rows_per_chunk` - Maximum rows per query (controls memory usage)
  - Time is automatically extracted from the time column and propagated on-graph
  - Date range is derived from `RunMode`/`RunFor` — no need to pass dates explicitly
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
- Time-based chunking with various chunk sizes
- WHERE clause preservation in chunked queries
- Time range filtering
- Edge cases: empty ranges, chunk sizes larger than data, very small chunks
- Uneven data distribution across time ranges

## Development Tips

- Always implement both `KdbSerialize` and `KdbDeserialize` for types used in round-trip scenarios
- **Time management**: Time is stored on-graph in tuples `(NanoTime, T)`, not in record structs
  - When reading: time is extracted from the KDB time column and passed as the first tuple element
  - When writing: time is extracted from the tuple and prepended to the serialized row
  - Your structs should ONLY contain business data (no time field)
- Write operations use K object functional queries: `(insert; `tablename; row_values)`
- Connection pooling: Each `kdb_read()` and `kdb_write()` opens its own connection

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
            // Skip column 0 (time) - handled by adapter
            // Use get_sym() for zero-allocation interned symbol access
            sym: row.get_sym(1, interner)?,
            price: row.get(2)?.get_float()?,
            qty: row.get(3)?.get_long()?,
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

### Memory Management for Large Queries

The `kdb_read()` function uses time-based chunking to handle large result sets:

**How it works:**
- Splits the time range into sequential chunks
- Each chunk queries at most `rows_per_chunk` rows
- Only one chunk is held in memory at a time
- Automatically adapts to uneven data distribution

**Choosing `rows_per_chunk`:**
- Small records (~100 bytes): 10,000 rows ≈ 1 MB
- Medium records (~500 bytes): 10,000 rows ≈ 5 MB
- Large records (~1 KB): 10,000 rows ≈ 10 MB
- Adjust based on available memory and record size

**Best practices:**
- For datasets < 100K rows: use 10,000-50,000 rows per chunk
- For datasets > 1M rows: use 10,000-20,000 rows per chunk
- Monitor memory usage and adjust as needed
- Larger chunks = fewer queries but more memory
- Smaller chunks = more queries but less memory

**Query construction:**
- Pagination uses `select[offset,N] ...` — the offset/count hint is injected directly into the
  `select` statement so KDB+ applies it at scan time (no materialisation of the full result)
- Falls back to `(offset;N) sublist query` for non-`select` queries (exec, functional forms, etc.)
- Optionally injects a date partition filter into the base query before pagination:
  - `RunFor::Duration` → `date within (start_date;end_date)` (bounded)
  - `RunFor::Forever` or `RunFor::Cycles` → `date >= start_date` (no upper bound)
- Preserves existing WHERE clauses, joins, and other query logic

**Example:**
```rust
// In-memory table: no date filter
let stream = kdb_read(conn.clone(), "select from trades where sym=`AAPL", "time", None::<&str>, 10000);

// Splayed/partitioned table: inject date filter for partition pruning
let stream = kdb_read(conn, "select from trades", "time", Some("date"), 10000);
```
