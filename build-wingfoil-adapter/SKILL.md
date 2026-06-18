---
name: build-wingfoil-adapter
description: >
  Contribute a new I/O adapter to wingfoil — especially a database adapter
  (Postgres, ClickHouse, DuckDB, Redis, …). Use when a contributor wants to add
  a source/sink that plugs into the wingfoil graph. Grounded in the real codebase
  layout; the kdb+ adapter is the working template for databases.
---

# Build a wingfoil I/O adapter

wingfoil is a Rust DAG engine for ultra-low-latency stream processing. Adapters
live at the **edges** of the graph: a **source** turns external I/O into a
graph stream, a **sink** writes a graph stream back out. The core stays pure
data-flow.

This skill walks you through adding one. It's written for the **call to arms**:
*"build your favorite DB I/O adapter."* kdb+ is in; Postgres / ClickHouse /
DuckDB / Redis aren't — yet. The kdb adapter
(`wingfoil/src/adapters/kdb/`) is your reference template.

> Heads up: there is also a fuller, maintainer-grade checklist at
> `.claude/commands/new-adapter.md` (Python bindings, CI, benches, self-review).
> This skill is the contributor on-ramp — get a working adapter + example +
> tests, then graduate to that command for the full polish.

---

## 0. The one rule that catches everyone

**Time lives on the graph, not in your records.**

Streams carry `(NanoTime, T)` — the engine owns time. Your record struct holds
**business data only**: no timestamp field. A DB source's job is to produce
`(NanoTime, Row)` tuples; the time comes from the row's timestamp column, mapped
onto the graph clock. Get this right and the same adapter works for **both**
backtest (`RunMode::HistoricalFrom`) and live (`RunMode::RealTime`) — that's the
whole point.

Other invariants:

- **No locks on the hot path.** Never lock a `Mutex`/`RwLock` inside `cycle()`.
  Locks are fine in factory functions, `start()/stop()/teardown()`, and
  background threads. Hand data to `cycle()` via the channel primitives
  (`produce_async`, `ReceiverStream`, `ChannelSender`).
- **No `.unwrap()` in production code.** Use `?` with `anyhow::Context`, or
  `.expect("invariant: WHY")` only when the error branch is truly unreachable.
  `.unwrap()` is allowed in `#[cfg(test)]` and doc examples.
- **Every on-graph type is `Element`** = `Debug + Clone + Default + 'static`,
  and `Send` for adapters.

---

## 1. Branch + feature flags

```bash
git checkout main && git pull origin main && git checkout -b <name>
```

`wingfoil/Cargo.toml` — two feature flags, dependency optional:

```toml
[features]
<name> = ["dep:<db-client-crate>", "async"]
<name>-integration-test = ["<name>", "dep:testcontainers"]

[dependencies]
<db-client-crate> = { version = "x.y", optional = true }
testcontainers    = { version = "0.27", features = ["blocking"], optional = true }
```

`testcontainers` goes in `[dependencies]` (optional), not `[dev-dependencies]` —
Cargo features can't gate dev-deps.

Register the module in `wingfoil/src/adapters/mod.rs`:

```rust
#[cfg(feature = "<name>")]
pub mod <name>;
```

---

## 2. File layout

```
wingfoil/src/adapters/<name>/
  mod.rs                # connection config, public types, module //! doc, re-exports
  read.rs               # source  (the _read / _sub fn)
  write.rs              # sink    (the _write / _pub fn)
  integration_tests.rs  # gated by <name>-integration-test
  CLAUDE.md             # design decisions + pre-commit notes
```

For databases use the `_read` / `_write` verbs (like kdb/csv), not `_sub`/`_pub`.

---

## 3. The source (`read.rs`)

A database read is **time-sliced**: you don't pull the whole table, you pull a
window at a time and let the engine pace replay. The kdb adapter is the model —
a `produce_async` source plus a caller-supplied query closure:

```rust
// real wingfoil kdb signature — copy this shape
pub fn <name>_read<T, F>(
    connection: <Name>Connection,
    period: std::time::Duration,                 // slice width, e.g. 1 hour
    query_fn: F,
) -> Rc<dyn Stream<Burst<T>>>
where
    T: Element + Send + <Name>Deserialize + 'static,
    F: FnMut((NanoTime, NanoTime), i32, usize) -> String + Send + 'static,
{
    produce_async(move |ctx: RunParams| {
        let start = ctx.start_time;
        let end = ctx.end_time();              // requires RunFor::Duration
        async move {
            Ok(async_stream::stream! {
                // for each [t0, t1) slice in [start, end):
                //   let q = query_fn((t0, t1), date, slice_idx);
                //   run the query, decode each row -> (NanoTime, T)
                //   yield Ok((time, row));
                // on fatal error: yield Err(anyhow::anyhow!("..."));
            })
        }
    })
}
```

Key points:

- Output type is `Rc<dyn Stream<Burst<T>>>` — a burst is the set of rows landing
  at the same instant.
- The **closure builds the query string** from the time window. This keeps the
  adapter SQL-agnostic: a Postgres adapter takes `"select … where ts >= $1 …"`,
  a ClickHouse adapter takes its own dialect — the contributor decides.
- Bail clearly if misused: kdb errors when `start_time == NanoTime::ZERO`
  (caller forgot `HistoricalFrom`) or when `RunFor::Forever` would make slicing
  unbounded. Mirror that.
- Define a `<Name>Deserialize` trait so each row type maps columns →
  `(NanoTime, Self)`. The timestamp column becomes the graph time; the rest are
  struct fields.

If your DB client is **async** (tokio), use `produce_async` as above. If it's
**blocking/poll-based**, use `ReceiverStream` to marshal rows from a dedicated OS
thread into the graph via a `ChannelSender`.

---

## 4. The sink (`write.rs`)

A write consumes a `Burst<Row>` stream and inserts. Async client → `consume_async`:

```rust
#[must_use]
pub fn <name>_write(
    conn: <Name>Connection,
    upstream: &Rc<dyn Stream<Burst<Row>>>,
) -> Rc<dyn Node> {
    upstream.consume_async(Box::new(move |mut source| async move {
        // connect once, then:
        // while let Some((_t, burst)) = source.next().await { insert each row }
        Ok(())
    }))
}
```

Expose a fluent trait so callers can chain `.<name>_write(conn)` on a stream
(see `KdbWriteOperators` / the kdb `write.rs`). Provide an impl for both
`Stream<Burst<Row>>` and `Stream<Row>` (auto-wrap single rows into a one-element
burst).

For a blocking client, implement `MutableNode` with `start`/`cycle`/`stop`
instead, opening the connection in `start()` and inserting `self.src.peek_value()`
each `cycle()`.

---

## 5. Example + tests + docs

- **Example:** `wingfoil/examples/<name>/main.rs` — seed data → `read` →
  transform → `write` → verify. Register it in `wingfoil/Cargo.toml`:

  ```toml
  [[example]]
  name = "<name>"
  required-features = ["<name>"]
  ```

  Add a `README.md` next to it following the kdb example's
  Setup / Run / Code / Output structure, and add a one-row entry to the
  "I/O adapters" tables in **both** `/README.md` (absolute GitHub links) and
  `wingfoil/examples/README.md` (relative links + a short snippet section).

- **Integration tests:** `integration_tests.rs`, gated
  `#[cfg(all(test, feature = "<name>-integration-test"))]`. Spin the DB with
  `testcontainers` (`SyncRunner`). Cover: connection refused, a read returns
  seeded rows, a write round-trips (verify with a direct client query).
  Containerised open-source DBs (Postgres/ClickHouse/DuckDB/Redis) make this
  easy — no commercial license needed, unlike kdb.

- **`mod.rs` `//!` doc:** one-line description, a Docker setup one-liner, and a
  minimal `ignore` snippet each for reading and writing.

- **`CLAUDE.md`:** record design decisions (slice strategy, column→time mapping,
  type constraints) and the pre-commit commands.

---

## 6. Pre-commit (must all pass)

```bash
cargo fmt --all
cargo lint            # default features
cargo lint-all        # all features — your code is behind a feature flag, so this is the one that checks it
cargo test --features <name>-integration-test -p wingfoil -- --test-threads=1
```

---

## 7. Verify against the engine's two modes

The acceptance test for a DB adapter is that **one graph runs both ways**:

```rust
// Backtest: replay history through the graph
graph.clone().run(RunMode::HistoricalFrom(start), RunFor::Duration(day))?;

// Production: same adapters, same graph, wall clock
graph.run(RunMode::RealTime, RunFor::Forever)?;
```

If both work without touching anything but the `run()` arguments, the adapter is
correct. Open a PR, link the good-first-issue, and ping for contributor
mentorship if you want a pairing pass on the first one.
