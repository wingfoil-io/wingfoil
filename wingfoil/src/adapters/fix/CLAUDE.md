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
`FixConnection::fix_sub(&["4001"])` creates a graph node that watches the status stream and
automatically sends `MarketDataRequest` messages once the session reaches `LoggedIn`.
`FixConnection::send(msg)` and `FixConnection::sender()` provide raw access for advanced
use cases (e.g. throttled sweeps, custom message types). `FixSenderNode` is a separate sink for
cases where a dedicated outbound connection is needed (e.g. order routing).

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
cargo clippy --workspace --all-targets --exclude wingfoil-python -- -D warnings
cargo test -p wingfoil -- fix::tests    # unit tests (no network)
```

Integration tests (requires LMAX credentials):
```bash
LMAX_USERNAME=xxx LMAX_PASSWORD=yyy \
  cargo test --features fix-integration-test -p wingfoil \
    -- fix::integration_tests --nocapture --test-threads=1
```
