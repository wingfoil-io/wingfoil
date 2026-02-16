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

- `kdb_read()` - Execute a q query and stream results with time-based chunking
  - Uses adaptive chunking to handle arbitrarily large result sets with bounded memory
  - Parameters:
    - `connection` - KDB connection configuration
    - `query` - Base q query (can include WHERE clauses)
    - `time_col` - Name of the time column for chunking (time is extracted automatically)
    - `rows_per_chunk` - Maximum rows per query (controls memory usage)
  - Time is automatically extracted from the time column and propagated on-graph
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
- User query is wrapped as subquery: `select[N] from (user_query) where time >= start and time <= end`
- Preserves existing WHERE clauses, joins, and other query logic
- Time-based filtering leverages KDB+'s time column indexing for efficiency

**Example:**
```rust
// Read 1M rows with 10K row chunks (bounded ~10 MB memory)
// Note: Trade struct does not contain time field
let stream = kdb_read(
    conn,
    "select from trades where sym=`AAPL",
    "time",    // Time column name - extracted automatically
    10000      // 10K rows per chunk
);
```
