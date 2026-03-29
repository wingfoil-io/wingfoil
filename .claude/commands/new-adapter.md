Implement a new I/O adapter for wingfoil named `$ARGUMENTS`. Follow these steps in order. Work test-driven: write each test before its implementation.

## 1. Branch

```bash
git checkout main && git pull origin main && git checkout -b $ARGUMENTS
```

## 2. Feature flags — `wingfoil/Cargo.toml`

Add two feature flags:
```toml
[features]
$ARGUMENTS = ["dep:some-client-crate", "async"]
$ARGUMENTS-integration-test = ["$ARGUMENTS", "dep:testcontainers", "dep:testcontainers-modules"]

[dependencies]
some-client-crate = { version = "x.y", optional = true }
testcontainers = { version = "0.23", optional = true }
testcontainers-modules = { version = "0.11", optional = true }
```

Note: `testcontainers` must go in `[dependencies]` as optional (not `[dev-dependencies]`) because Cargo feature flags cannot gate dev-deps.

## 3. Module registration — `wingfoil/src/adapters/mod.rs`

```rust
#[cfg(feature = "$ARGUMENTS")]
pub mod $ARGUMENTS;
```

## 4. Docker image / container setup

Choose an official or well-maintained image for the service. Prefer `testcontainers-modules` if a module exists; otherwise use `GenericImage`:

```rust
// In integration_tests.rs
use testcontainers::runners::AsyncRunner;

// Option A — if testcontainers-modules has a module:
use testcontainers_modules::some_service::SomeService;
let container = SomeService::default().start().await?;
let port = container.get_host_port_ipv4(DEFAULT_PORT).await?;

// Option B — GenericImage fallback:
use testcontainers::GenericImage;
let container = GenericImage::new("vendor/image", "tag")
    .with_env_var("KEY", "value")
    .start()
    .await?;
let port = container.get_host_port_ipv4(DEFAULT_PORT).await?;
```

The container is stopped automatically when dropped. No `docker-compose.yml` needed.

## 5. File structure

```
wingfoil/src/adapters/$ARGUMENTS/
  mod.rs               # Connection config, public types, re-exports
  read.rs              # sub function (produce_async)
  write.rs             # pub function (consume_async)
  integration_tests.rs # gated by $ARGUMENTS-integration-test feature
  CLAUDE.md            # documents design decisions and pre-commit requirements
```

## 6. Types — `mod.rs`

All types used on-graph must satisfy `Element = Debug + Clone + Default + 'static` and be `Send`.

```rust
pub struct <Name>Connection { /* endpoint, credentials, etc. */ }

// Value type for the pub (consumer) input
#[derive(Debug, Clone, Default)]
pub struct <Name>Kv { pub key: String, pub value: Vec<u8> }

// Event type for the sub (producer) output — include a Default variant
#[derive(Debug, Clone, Default)]
pub struct <Name>Event { /* fields */ }
```

## 7. Sub method — `read.rs` (producer)

Uses `produce_async`. Returns `Rc<dyn Stream<Burst<Event>>>`.

```rust
pub fn $ARGUMENTS_sub(conn: <Name>Connection, /* params */) -> Rc<dyn Stream<Burst<<Name>Event>>> {
    produce_async(move |_ctx: RunParams| async move {
        Ok(async_stream::stream! {
            // connect, snapshot, then live stream
            // yield Ok((NanoTime::now(), event))
            // yield Err(anyhow::anyhow!("...")) on fatal error
        })
    })
}
```

If the service supports a **snapshot + watch** pattern (like etcd), use watch-before-get to avoid races:
1. Open watch/subscribe first
2. Read snapshot, capture its revision/cursor
3. Emit snapshot events
4. Emit watch events, skipping any with revision <= snapshot revision

## 8. Pub method — `write.rs` (consumer)

Uses `consume_async`. Returns `Rc<dyn Node>`.

```rust
pub fn $ARGUMENTS_pub(conn: <Name>Connection, upstream: &Rc<dyn Stream<Burst<<Name>Kv>>>) -> Rc<dyn Node> {
    upstream.consume_async(Box::new(move |source: Pin<Box<dyn FutStream<Burst<<Name>Kv>>>>| {
        async move {
            // connect once
            // while let Some((_time, burst)) = source.next().await { write each kv }
            Ok(())
        }
    }))
}

// Fluent extension trait
pub trait <Name>PubOperators {
    fn $ARGUMENTS_pub(self: &Rc<Self>, conn: <Name>Connection) -> Rc<dyn Node>;
}
impl <Name>PubOperators for dyn Stream<Burst<<Name>Kv>> { ... }
```

## 9. Integration tests — `integration_tests.rs`

Gate with `#[cfg(all(test, feature = "$ARGUMENTS-integration-test"))]`.

Write tests in this order (connection refused first — no container needed):

1. **`test_connection_refused`** — error propagates correctly
2. **`test_sub_snapshot`** — pre-seeded data appears in snapshot phase
3. **`test_sub_live_updates`** — events arrive after snapshot
4. **`test_pub_round_trip`** — `pub` writes → verify via direct client read
5. **`test_sub_no_race`** — concurrent write during snapshot→watch handoff not missed or duplicated (if applicable)
6. **`test_delete_events`** — delete/tombstone events handled correctly (if applicable)

Test structure — separate async setup from sync graph execution (matches KDB pattern):

```rust
#[test]
fn test_sub_snapshot() -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let (endpoint, _container) = rt.block_on(start_container())?;
    rt.block_on(seed_data(&endpoint, &[("key", "val")]))?;

    let conn = <Name>Connection::new(&endpoint);
    let collected = $ARGUMENTS_sub(conn, "prefix").collapse().collect();
    collected.clone().run(RunMode::RealTime, RunFor::Cycles(1))?;

    assert_eq!(collected.peek_value().len(), 1);
    Ok(())
}
```

## 10. Example — `wingfoil/examples/$ARGUMENTS/main.rs`

Show a realistic end-to-end use: seed data → `sub` → transform → `pub` → verify.

Register in `wingfoil/Cargo.toml`:
```toml
[[example]]
name = "$ARGUMENTS"
required-features = ["$ARGUMENTS"]
```

## 11. CLAUDE.md — `wingfoil/src/adapters/$ARGUMENTS/CLAUDE.md`

Document:
- Module structure
- Key design decisions (especially any snapshot/watch race prevention)
- Pre-commit requirements (integration test command, fmt, clippy)
- Any gotchas (API version pins, type constraints, etc.)

## 12. CI — add to release workflow

Add a job to `.github/workflows/release.yml` **parallel** to `grafana-integration`:

```yaml
$ARGUMENTS-integration:
  name: $ARGUMENTS Integration Tests
  needs: preflight
  runs-on: ubuntu-latest
  steps:
    - uses: actions/checkout@v4
    - name: Install Rust Toolchain
      uses: actions-rs/toolchain@v1
      with:
        toolchain: stable
        override: true
    - name: Cache Rust Build Artifacts
      uses: Swatinem/rust-cache@v2
    - name: Run $ARGUMENTS integration tests
      run: |
        cargo test --features $ARGUMENTS-integration-test -p wingfoil \
          -- --test-threads=1 --nocapture
      env:
        RUST_LOG: INFO
```

Update the `tag` job's `needs` to include `$ARGUMENTS-integration`.

Also create a standalone `.github/workflows/$ARGUMENTS-integration.yml` with `on: workflow_dispatch` containing the same job (for manual runs outside of release).

## 13. Pre-commit checklist

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
cargo test --features $ARGUMENTS-integration-test -p wingfoil -- --test-threads=1
```

All three must pass before committing.
