# etcd Adapter

Streams a key-prefix snapshot + live watch from etcd (`etcd_sub`) and writes
key-value pairs to etcd (`etcd_pub`).

## Module Structure

```
etcd/
  mod.rs               # EtcdConnection, EtcdEntry, EtcdEvent, public re-exports
  read.rs              # etcd_sub() producer
  write.rs             # etcd_pub() consumer, EtcdPubOperators trait
  integration_tests.rs # Integration tests (requires Docker, gated by feature)
  CLAUDE.md            # This file
```

## Key Components

### Reading from etcd — `etcd_sub`

- `etcd_sub(conn, prefix)` — produces `Burst<EtcdEvent>`
  - **Phase 1:** emits a snapshot of all current KVs matching the prefix as `EtcdEventKind::Put`
  - **Phase 2:** streams live watch events (Put and Delete)
  - The watch is opened **before** the GET to guarantee no writes are missed in the handoff window
  - Watch events with `mod_revision ≤ snapshot_rev` are filtered out to prevent duplicates

### Writing to etcd — `etcd_pub`

- `etcd_pub(conn, upstream, lease_ttl, force)` — consumes `Burst<EtcdEntry>`, issues one PUT per entry
- `EtcdPubOperators::etcd_pub(conn, lease_ttl, force)` — fluent API, implemented for both
  `Rc<dyn Stream<Burst<EtcdEntry>>>` and `Rc<dyn Stream<EtcdEntry>>` (single entries are auto-wrapped)
- Pass `lease_ttl: None` for plain writes (keys persist until deleted)
- Pass `lease_ttl: Some(Duration)` to attach an etcd lease with automatic keepalive renewal;
  the lease is revoked on clean shutdown so keys vanish immediately
- `force: true` silently overwrites existing keys; `force: false` issues a conditional
  transaction (`create_revision == 0`) and errors if the key already exists

### Types

- `EtcdConnection::new(endpoint)` — single endpoint (e.g. `"http://localhost:2379"`)
- `EtcdConnection::with_endpoints(iter)` — cluster endpoints
- `EtcdEntry { key: String, value: Vec<u8> }` — plain key-value pair; `.value_str()` for UTF-8
- `EtcdEvent { kind: EtcdEventKind, entry: EtcdEntry, revision: i64 }` — watch event

## Snapshot → Watch Handoff (Race Prevention)

```
Thread:  WATCH(prefix) ──→ GET(prefix, snapshot_rev) ──→ emit snapshot ──→ drain watch_stream
                                                                           (skip mod_rev ≤ snapshot_rev)
```

Any write committed between WATCH registration and GET completion will appear in
`watch_stream` with `mod_revision > snapshot_rev` — never missed.
Any write already visible in the GET has `mod_revision ≤ snapshot_rev` — filtered
out as a duplicate.

## Pre-Commit Requirements

1. **Run integration tests (requires Docker):**

   ```bash
   cargo test --features etcd-integration-test -p wingfoil \
     -- --test-threads=1 etcd::integration_tests
   ```

2. **Run standard checks:**

   ```bash
   cargo fmt --all
   cargo lint        # default features
   cargo lint-all    # all features
   ```

## Integration Test Details

Tests use `testcontainers` (`SyncRunner`) to start a `gcr.io/etcd-development/etcd:v3.5.0`
container per test. Docker must be running. No environment variables required.

Feature flag: `etcd-integration-test` (implies `etcd`).

Tests must be run with `--test-threads=1` to avoid port conflicts between containers.

### Test coverage

| Test | What it proves |
|------|----------------|
| `test_connection_refused` | Error propagates when etcd is unreachable |
| `test_sub_empty_snapshot_then_live_write` | Empty snapshot + live event works correctly |
| `test_sub_snapshot_with_existing_keys` | Pre-existing keys appear in snapshot phase |
| `test_sub_live_updates` | Live events arrive after snapshot |
| `test_pub_round_trip` | `etcd_pub` writes are readable via direct client |
| `test_pub_lease_keys_expire_after_revoke` | Leased keys vanish immediately when consumer stops |
| `test_pub_lease_keepalive_extends_ttl` | Keepalive renews lease so keys survive past raw TTL |
| `test_pub_no_lease_keys_persist` | `None` lease: keys remain after consumer stops |
| `test_pub_force_true_overwrites` | `force: true` silently overwrites an existing key |
| `test_pub_force_false_fails_if_exists` | `force: false` errors (naming the key) and leaves the original value |
| `test_pub_force_false_succeeds_if_absent` | `force: false` writes normally when the key is absent |
| `test_sub_delete_events` | `EtcdEventKind::Delete` is emitted correctly |
| `test_sub_no_race_between_snapshot_and_watch` | Concurrent write not missed or duplicated |
| `test_etcd_kv_value_str` | `EtcdEntry::value_str()` UTF-8 accessor (no container needed) |

## Notes

- `etcd-client` is pinned to `"0.18"`. The API changed significantly between versions.
- There is no ready-made etcd testcontainers module; tests use `testcontainers` (0.27,
  `blocking` feature) with `GenericImage` and `gcr.io/etcd-development/etcd` directly.
- `etcd_sub` is designed for `RunMode::RealTime`. Using it in `HistoricalFrom` mode is
  technically valid but timestamps will be wall-clock `NanoTime::now()`, not historical.
