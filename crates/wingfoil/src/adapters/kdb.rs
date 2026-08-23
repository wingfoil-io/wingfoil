//! kdb adapter — KDB+/q connectivity: time-partitioned historical **reads**
//! ([`kdb_read`], and its file-cached twin [`kdb_read_cached`]), a real-time
//! tickerplant **subscription** ([`kdb_sub`]), and a streaming insert **sink**
//! ([`KdbSinkOps::kdb_write`]), on the async `kdbplus` IPC client (`QStream`).
//! It ports the legacy `wingfoil::adapters::kdb` module onto the Op model.
//!
//! Time is carried **on-graph** in tuples `(NanoTime, T)`, never inside the
//! record struct: on read the [`KdbDeserialize`] impl extracts it from a
//! timestamp column into the tuple; on write it is prepended as the first
//! inserted column. Your struct holds only business data (sym, price, qty, …).
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`statistics`](crate::adapters::statistics)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly (`use wingfoil::adapters::kdb::*;`):
//!
//! - **Sources** — the free builder functions [`kdb_read`] / [`kdb_read_cached`]
//!   (bounded historical replay, one query per time slice) and [`kdb_sub`] (a
//!   real-time tickerplant tail) on a [`GraphBuilder`](crate::fluent::GraphBuilder), emitting
//!   `Stream<Burst<T>>`.
//! - **Sink** — the [`KdbSinkOps`] extension trait on `Stream<Burst<T>>`,
//!   enabled with `use wingfoil::adapters::kdb::KdbSinkOps;`.
//! - **Serde traits** — [`KdbDeserialize`] (row → `(NanoTime, T)`) for the
//!   sources and [`KdbSerialize`] (`T` → row K object) for the sink.
//!
//! # Reading (time-sliced historical replay)
//!
//! [`kdb_read`] splits the run's `[start, end)` window (from
//! `RunMode::HistoricalFrom` + `RunFor::Duration`) into contiguous, half-open,
//! midnight-aligned slices of length `period` via the shared
//! [`compute_validated_time_slices`](crate::adapters::common) slicer (the same
//! routine the postgres reader uses), and calls `query_fn` once per slice with
//! `((t0, t1), date, iteration)` — a half-open `[t0, t1)` window, the KDB-style
//! `date` integer (**days since 2000-01-01**), and the slice index within that
//! day. The caller builds the whole query (date / partition hints / a
//! `time >= t0j, time < t1j` filter for clean round-number boundaries). The
//! window is validated + sliced at **wiring** (a pure check, no I/O); the
//! connect and slice queries then run at the **start of the run** via
//! [`produce_async`](crate::async_source::produce_async), which replays the
//! decoded, in-window rows at their timestamps — deterministic, with no network
//! I/O at graph construction.
//!
//! Because the first slice begins at the period boundary at or before
//! `start_time`, a `time >= t0j` filter can legitimately return rows earlier
//! than `start_time` (and the final slice's `t1` can reach past `end_time`).
//! Rows outside the run's `[start_time, end_time)` window are **dropped** with a
//! per-slice warning via the shared [`WindowFilter`](crate::adapters::common)
//! rather than emitted — delivering a row before the monotonic graph clock would
//! abort the run. Rows sharing a timestamp ride **one** `Burst<T>`; iterate the
//! burst to process every row (`.collapse()` keeps only the last per tick). A
//! non-monotonic timestamp aborts the run (add `xasc` to the query).
//!
//! `prev_time` is reset each slice so time-of-day columns work across date
//! partitions (timestamps restart at midnight on each new date).
//!
//! [`kdb_read_cached`] is the same reader with a file cache
//! ([`CacheConfig`]/[`FileCache`](crate::adapters::cache)) checked before each
//! slice query: a hit is served without opening a TCP connection; a miss queries
//! KDB and writes the full `[t0, t1)` slice to disk. The window clamp is applied
//! on emit on **both** hits and misses (the cache key is the query string, which
//! does not encode `start_time`/`end_time`, so the cache stores the full slice).
//! `T` must additionally be `serde::Serialize + Deserialize + Sync`.
//!
//! # Subscribing (real-time tickerplant tail)
//!
//! [`kdb_sub`] subscribes to a q **tickerplant** with `.u.sub[`table;syms]` and
//! streams rows as they are pushed — genuinely push-based (unlike postgres's
//! `LISTEN`/`NOTIFY` re-query, the tickerplant pushes the rows themselves). It is
//! a live, unbounded, wall-clock-stamped stream with no historical timeline to
//! replay, so it **rejects `RunMode::HistoricalFrom` at wiring time** (use
//! [`kdb_read`] for historical replay) and runs under [`RunMode::RealTime`](wingfoil::RunMode) only.
//! It tails from the moment of subscription — it does **not** replay the
//! tickerplant log / RDB buffer. Non-`upd` control messages (heartbeats, the
//! end-of-day `.u.end`) are ignored.
//!
//! # Sink
//!
//! [`KdbSinkOps::kdb_write`] connects lazily on the first write (on the consumer
//! task, so a connection error surfaces during the run, not at wiring) and
//! inserts each burst as one row-batch via a functional
//! `insert[`table; (time_col; col1; …)]` query, the graph timestamp prepended as
//! the first column. Writes are driven off the graph thread with
//! [`consume_async`](crate::async_source::consume_async). Non-finite floats map
//! to q's native null/infinity literals (`0n`/`0w`/`0Ne`/…); symbols are built
//! via the `` `$"…" `` string cast so special characters (`-`, spaces, …) are
//! representable. Serialized columns must be **scalar atoms** — vector/nested
//! columns are read-only (see [`KdbSerialize`]).
//!
//! # Deviations from legacy
//!
//! Every legacy *capability* — the time-sliced read, its cached twin, the
//! tickerplant subscription, the streaming write, the `KdbDeserialize` /
//! `KdbSerialize` / `KdbExt` traits, [`Sym`]/[`SymbolInterner`], and the
//! [`Row`]/[`Rows`] row access — is preserved. The surface differs in these
//! deliberate ways, mirroring the [`postgres`](crate::adapters::postgres) /
//! [`redis`](crate::adapters::redis) / [`kafka`](crate::adapters::kafka) ports:
//!
//! 1. **The graph owns the tokio runtime.** No factory takes a `&Handle`: the
//!    [`GraphBuilder`](crate::fluent::GraphBuilder) owns one runtime, created lazily on first async use and
//!    dropped at teardown, shared by every async adapter (see
//!    `docs/decisions/runtime-ownership.md`; embed in your own runtime with
//!    [`GraphBuilder::with_async_runtime`](crate::fluent::GraphBuilder::with_async_runtime)).
//!    [`kdb_read`] / [`kdb_read_cached`] take a [`RunParams`](crate::async_source::RunParams) (they need the run's
//!    `[start, end)` window to slice queries at wiring — a pure check); the live
//!    [`kdb_sub`] takes only a [`RunMode`](wingfoil::RunMode) (to reject a historical run at wiring).
//!    The **sink** drives its client with `block_on` at teardown, so the graph
//!    must be built, run, and dropped from a **non-async thread** (`main`, a
//!    `#[test]` fn).
//! 2. **The reader defers its connect + queries to the run, and streams them
//!    lazily.** Both wingfoil and legacy run [`kdb_read`] through
//!    [`produce_async`](crate::async_source::produce_async), so wiring does no
//!    I/O and a connection / query / decode / non-monotonic-time error aborts the
//!    *run*, not graph construction. The window is still validated + sliced at
//!    wiring. Slices are queried **lazily, one at a time** (an `async_stream`
//!    generator, legacy's `chunk_stream` shape), so with a `buffer_size` bound
//!    the replay stays bounded in memory and pipelines KDB I/O with graph
//!    compute — legacy's model, not an up-front collection.
//! 3. **The sink is a trait only.** Legacy exposed a free `kdb_write` fn *and* a
//!    `KdbWriteOperators` trait; wingfoil folds the entry point into [`KdbSinkOps`],
//!    which connects lazily inside the `consume_async` consumer on the first
//!    write (so wiring opens no socket; a connect failure surfaces during the
//!    run).
//! 4. **The live subscription rejects historical at wiring** (register B2,
//!    ratified — a live, unbounded tickerplant tail with no bounded historical
//!    twin). Legacy checked the same guard inside its `produce_async` closure
//!    (at run start); wingfoil moves it to wiring for a clearer fail-fast.
//! 5. **`buffer_size` on [`kdb_read`] is honoured as back-pressure** (like
//!    legacy): `Some(n)` bounds the replay to ~`n` timestamp-groups of
//!    look-ahead — the lazy per-slice source is fetched only as the graph drains,
//!    so memory stays bounded and I/O pipelines with compute; `None` is unbounded.
//!    [`kdb_read_cached`] keeps legacy's cache-in-place-of-`buffer_size`
//!    signature (it rides the unbounded [`produce_async`], as legacy did) while
//!    still streaming its slices lazily.
//!
//! # Setup
//!
//! The KDB+ integration tests need a running q instance (KDB+ has no public,
//! freely-licensed container image):
//!
//! ```sh
//! q -p 5000
//! ```

mod read;
mod read_cached;
mod sub;
mod write;

pub use read::{KdbDeserialize, KdbExt, Row, RowIter, Rows, kdb_read};
pub use read_cached::kdb_read_cached;
pub use sub::kdb_sub;
pub use write::{KdbSerialize, KdbSinkOps};

/// Re-export of the file cache config used by [`kdb_read_cached`].
pub use crate::adapters::cache::CacheConfig;

/// Re-export of the `kdbplus` error type, so callers can name it in
/// [`KdbDeserialize`] impls without depending on `kdbplus` directly.
pub use kdb_plus_fixed::ipc::error::Error as KdbError;

/// Re-export of the `kdbplus` `K` object type, for building custom
/// serialization in [`KdbSerialize`] impls.
pub use kdb_plus_fixed::ipc::K;

/// KDB type codes, for a caller decoding a column whose type is only known at
/// runtime — a dynamic reader such as the Python binding, which has no record
/// `struct` to deserialize into and must dispatch on the value's actual type.
/// Re-exported alongside [`K`] so such a caller needs no direct dependency on
/// the underlying IPC crate.
pub use kdb_plus_fixed::qtype;

/// The interned symbol type, and the interner that deduplicates it.
///
/// These now live in [`adapters::common`](crate::adapters::common) — the
/// market data vocabulary needed the same pair, and a second copy would have
/// been a near-duplicate of this one. Re-exported here so `kdb::Sym` and
/// `kdb::SymbolInterner` keep resolving; an interner is still created once per
/// read call and passed to `from_kdb_row` / [`Row::get_sym`].
pub use crate::adapters::common::{Sym, SymbolInterner};

/// KDB connection configuration.
#[derive(Debug, Clone)]
pub struct KdbConnection {
    /// Host address of the KDB server.
    pub host: String,
    /// Port number of the KDB server.
    pub port: u16,
    /// Optional authentication credentials.
    pub credentials: Option<KdbCredentials>,
}

/// Authentication credentials for a KDB connection.
#[derive(Debug, Clone)]
pub struct KdbCredentials {
    /// Username for authentication.
    pub username: String,
    /// Password for authentication.
    pub password: String,
}

impl KdbConnection {
    /// Create a new KDB connection configuration.
    pub fn new(host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            credentials: None,
        }
    }

    /// Add authentication credentials to the connection.
    #[must_use]
    pub fn with_credentials(
        mut self,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        self.credentials = Some(KdbCredentials {
            username: username.into(),
            password: password.into(),
        });
        self
    }

    /// The `"username:password"` string handed to `QStream::connect`, or an empty
    /// string when no credentials are set.
    ///
    /// This value carries the password and is used **only** at the connect call
    /// site — never in an error message or log. Error context uses
    /// [`redacted`](Self::redacted) instead.
    #[must_use]
    pub fn credentials_string(&self) -> String {
        match &self.credentials {
            Some(creds) => format!("{}:{}", creds.username, creds.password),
            None => String::new(),
        }
    }

    /// A `host:port` connection label safe for error messages and logs — the
    /// password is **never** included.
    ///
    /// Used at every `connect()` error site so a KDB password can never reach a
    /// log or an aborted-run error (the credential-redaction rule shared with the
    /// postgres adapter).
    #[must_use]
    pub fn redacted(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kdb_connection_new() {
        let conn = KdbConnection::new("localhost", 5000);
        assert_eq!(conn.host, "localhost");
        assert_eq!(conn.port, 5000);
        assert!(conn.credentials.is_none());
    }

    #[test]
    fn test_kdb_connection_with_credentials() {
        let conn = KdbConnection::new("localhost", 5000).with_credentials("user", "pass");
        let creds = conn.credentials.as_ref().unwrap();
        assert_eq!(creds.username, "user");
        assert_eq!(creds.password, "pass");
    }

    #[test]
    fn test_credentials_string() {
        let conn = KdbConnection::new("localhost", 5000);
        assert_eq!(conn.credentials_string(), "");

        let conn = KdbConnection::new("localhost", 5000).with_credentials("user", "pass");
        assert_eq!(conn.credentials_string(), "user:pass");
    }

    #[test]
    fn test_redacted_never_leaks_password() {
        let conn = KdbConnection::new("localhost", 5000).with_credentials("user", "s3cr3t");
        let out = conn.redacted();
        assert!(!out.contains("s3cr3t"), "password leaked: {out}");
        assert_eq!(out, "localhost:5000");
    }

    #[test]
    fn test_sym_display_and_default() {
        assert_eq!(Sym::default().to_string(), "");
        let mut interner = SymbolInterner::default();
        let a = interner.intern("AAPL");
        let b = interner.intern("AAPL");
        assert_eq!(a, b);
        assert_eq!(a.to_string(), "AAPL");
        // Interned equal values share the same allocation. `Sym` now lives in
        // `adapters::common`, so this goes through its public `ptr_eq` rather
        // than reaching into the newtype; the interning behaviour kdb relies on
        // is unchanged, and `common`'s own tests cover it more fully.
        assert!(a.ptr_eq(&b));
    }
}
