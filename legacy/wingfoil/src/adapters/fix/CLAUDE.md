# FIX Adapter

## Module Structure

```
wingfoil/src/adapters/fix/
  mod.rs               # All types, codec, session logic, node impls, public API
  integration_tests.rs # LMAX London Demo integration tests (requires credentials)
```

The adapter is a single `mod.rs` rather than the `read.rs`/`write.rs` split used by async
adapters. This is intentional: FIX sessions are stateful, bidirectional TCP connections where
read and write share a `FixSession` state machine (sequence numbers, heartbeat tracking).
Splitting would require threading the session state across files with no benefit.

## Design Decisions

### Synchronous poll-based architecture

Unlike async adapters (etcd, ZMQ) that use `produce_async`/`consume_async`, the FIX adapter
uses custom `MutableNode` implementations polled by the graph engine. Two poll modes:

- **`AlwaysSpin`** — graph spin loop drives non-blocking socket reads (~1–5 µs latency)
- **`Threaded`** — background thread with channel bridge (~10–100 µs latency)

This avoids the overhead of an async runtime for a protocol where microsecond latency matters.

### FixConnection and fix_sub

`fix_connect_tls` returns a `FixConnection` bundling the data/status streams and session handle.
`FixConnection::fix_sub(symbols)` takes a `Rc<dyn Stream<Vec<String>>>` of symbol IDs (e.g.
`constant(vec!["4001".into()])`) and creates a graph node that watches the status stream and
automatically sends `MarketDataRequest` messages once the session reaches `LoggedIn`; symbols
arriving before logon are queued and subscribed on login.
`FixConnection::send(msg)` and `FixConnection::sender()` provide raw access for advanced
use cases (e.g. throttled sweeps, custom message types). `FixOperators::fix_send` (backed by the
private `FixSenderNode`) is a separate sink for cases where a dedicated outbound connection is
needed (e.g. order routing).

### Initiator reconnect after a session drop

In `Threaded` mode (which `fix_connect_tls`/`fix_connect_tls_logon` always use), an initiator
whose *established* session drops reconnects instead of giving up: the session thread pauses
`RECONNECT_DELAY` (500 ms) so a flapping venue isn't hammered, then re-connects with a fresh
`FixSession` that re-logs-in. Status consumers see `Disconnected` then a new `LoggedIn` —
`fix_sub` does not re-send subscriptions on relogin, so resubscription logic must key off
`LoggedIn` itself if needed. Initial connect *failures* still give up (an `Error` status is
emitted); acceptors loop to re-accept. `AlwaysSpin` initiators do not reconnect. Covered by the
`initiator_reconnects_after_a_session_drop` unit test.

### Pluggable Logon authentication (`FixLogon`)

`fix_connect_tls` still takes a `password: Option<&str>` (LMAX-style tag 553/554). For
venues that authenticate differently, `fix_connect_tls_logon` takes a `FixLogon`:

- `FixLogon::None` — defaults only (EncryptMethod/HeartBtInt/ResetSeqNumFlag).
- `FixLogon::Password(..)` — Username (553) + Password (554).
- `FixLogon::custom(builder)` — the builder is handed a `LogonContext` (SenderCompID,
  TargetCompID, MsgSeqNum, and the exact SendingTime string the Logon carries) and returns
  extra tag/value pairs. This is the seam Binance's Ed25519 signer uses: it signs tags
  35/49/56/34/52 joined by SOH and returns RawData (96) + RawDataLength (95) + Username (553).

The signing payload must match the bytes on the wire, so `FixSession::send_with` computes
the sequence number and SendingTime **once** and passes both to the builder and the encoder.
`SendingTime` is formatted with millisecond precision (`YYYYMMDD-HH:MM:SS.sss`). wingfoil
stays free of any venue/crypto specifics — the Ed25519 implementation lives in the caller
(e.g. the kes `binance` adapter).

### No testcontainers

There is no standard Docker image for a FIX engine. Integration tests connect to the LMAX
London Demo (free account required) and are gated behind environment variables.

### Custom FIX codec

The adapter includes a hand-written FIX 4.4 tag-value codec (`encode_message`, `decode_fields`,
`build_message`, `find_message`). No third-party FIX codec crate is used. If dictionary-driven
validation is wanted later, evaluate and add a maintained crate at that point — do not carry an
unused dependency for it.

## Pre-commit Requirements

```bash
cargo fmt --all
cargo lint        # default features
cargo lint-all    # all features
cargo test -p wingfoil --features fix -- fix::tests    # unit tests (no network)
```

Integration tests (requires LMAX credentials):
```bash
LMAX_USERNAME=xxx LMAX_PASSWORD=yyy \
  cargo test --features fix-integration-test -p wingfoil \
    -- fix::integration_tests --nocapture --test-threads=1
```
