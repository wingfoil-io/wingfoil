# FIX Adapter (wingfoil)

The FIX (Financial Information eXchange) protocol: a synchronous, poll-based
session engine — initiator and acceptor, plain TCP or TLS. Ports legacy
`wingfoil::adapters::fix` onto the Op model.

This port is where the **busy-spin `custom_node`** shape was worked out (and
where the engine bug it exposed, register **A7**, was fixed).

## Layout

```
adapters/
  fix.rs           # codec, session state machine, both poll modes, sources, sender, sink
  fix/CLAUDE.md    # this file
```

Everything is in one file: the codec, the `FixSession` state machine,
`SpinState`, the threaded session loop, and the public factories.

## Feature gating

```toml
fix = ["dep:rustls", "dep:webpki-roots", "dep:kanal", "dep:chrono"]
fix-integration-test = ["fix"]
```

**No `async`** — deliberately, matching legacy. `rustls`/`webpki-roots` back
the TLS initiator (crypto provider `ring`, same as [`web`](../web/CLAUDE.md));
`kanal` backs the lock-free outbound inject channel.

## Entry points

| Item | Kind | Returns |
|---|---|---|
| `fix_connect(g, run_mode, host, port, sender_comp_id, target_comp_id, mode)` | source | `(Stream<Burst<FixMessage>>, Stream<Burst<FixSessionStatus>>)` |
| `fix_accept(g, run_mode, port, sender_comp_id, target_comp_id, mode)` | source | the same pair |
| `fix_connect_tls(g, run_mode, host, port, sender, target, password)` | source | `FixConnection` |
| `fix_connect_tls_logon(g, run_mode, host, port, sender, target, logon)` | source | `FixConnection` |
| `FixConnection::fix_sub(symbols)` | helper | `Stream<()>` — declarative market-data subscription |
| `FixConnection::sender()` / `send()` | handle | `FixSender`, a lock-free bounded inject queue |
| `FixOperators::fix_send(host, port, sender, target)` | sink trait on `Stream<FixMessage>` | `Result<Stream<()>>` |

## What to know before changing it

- **Two poll modes, selected by `FixPollMode`:**
  - `AlwaysSpin` — a busy-spin `custom_node` doing non-blocking socket reads on
    the graph thread (~1–5 µs, one core pinned). **No TLS.** No reconnect.
  - `Threaded` (default for `fix_connect_tls*`) — a background OS thread runs
    the session loop and feeds a `channel` (~10–100 µs, shares CPU). The **only
    mode that reconnects** after an established session drops.
- **`g.poll` was too narrow, hence `custom_node`.** A spin FIX source needs a
  deferred connect at `start()`, a **fallible** cycle (`?` a read error into a
  run abort), and a teardown hook (Logout) — none of which `g.poll`'s
  `Fn() -> Option<T>` closure offers. `g.custom_node(&[], &[],
  Activation::ALWAYS, cycle)` is the general twin; the start hook is attached
  with `compose_spawn_at_start` on the node's index. **`custom_node` now sets
  the engine's `has_always` busy-spin flag** — it did not before this port
  (register A7), so an `ALWAYS` custom node was never driven each cycle.
- **Same-process wiring order matters.** Start hooks fire in **wiring order**,
  so in a loopback graph the **acceptor must be wired first** — its listener
  has to be bound before the initiator's synchronous connect runs in its own
  start hook. `tests/fix_integration.rs` depends on this.
- **Both modes are realtime-only, rejected at wiring** with a "real-time"
  error. Legacy checked real-time-ness at run `start()`; next rejects earlier.
  The message is the same.
- **Data and status are multiplexed in-band** over one transport (the internal
  `FixEvent` envelope) and split before reaching the caller, so a `LoggedIn`
  transition stays ordered relative to the messages around it.
- **Reconnect semantics (legacy-exact):** a `Threaded` initiator whose
  *established* session drops reconnects after `RECONNECT_DELAY` (so a flapping
  venue isn't hammered); acceptors loop to re-accept; **initial connect
  failures give up** and emit `FixSessionStatus::Error`; `AlwaysSpin`
  initiators do not reconnect at all.
- **Two outbound paths, don't confuse them.** `fix_send` opens its *own*
  outbound session (connect + logon at `start()`, realtime-only) and writes
  from the graph thread, back-pressured by the kernel TCP send buffer.
  `FixSender` (from `FixConnection::sender()`) injects into an **established**
  session from outside the graph, over a lock-free bounded `kanal` queue
  drained by the `Threaded` session thread — that is what `fix_sub` uses for
  its MarketDataRequests.
- **Custom Logon auth lives in the caller.** `fix_connect_tls` takes a
  `password: Option<&str>` (LMAX-style tags 553/554). `fix_connect_tls_logon`
  takes a `FixLogon`; `FixLogon::custom` hands a builder the `LogonContext`
  (SenderCompID / TargetCompID / MsgSeqNum / SendingTime) so it can attach a
  signature bound to the exact Logon header (e.g. Binance's Ed25519 `RawData`,
  tag 96, over tags 35/49/56/34/52 joined by SOH). **Keep wingfoil free of
  venue/crypto specifics.**
- **Teardown costs up to one 200 ms read timeout.** The threaded session loop
  checks a stop flag against its read timeout (the zmq pattern) rather than
  legacy's `Arc<Mutex<Option<TcpStream>>>` shutdown handle — no lock on the
  graph path.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `fix.rs` — three
items: source factories take a `GraphBuilder` + `RunMode` and reject historical
at wiring; sources return `Stream`s (and `fix_send` a `Result<Stream<()>>`,
`fix_sub` a `Stream<()>`) rather than `Rc<dyn Stream>` / `Rc<dyn Node>`; and
the no-lock threaded teardown above. Everything else — codec, session state
machine, field/tag semantics — is a verbatim port. There is no single-value
convenience sink impl (the element is `FixMessage`, not a `Burst`).

Legacy's **credentialed LMAX-demo integration tests are not ported**; the
`fix-integration-test` feature covers the same-process loopback tests instead.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/fix_adapter.rs` | `#![cfg(feature = "fix")]` | nothing |
| `tests/fix_integration.rs` | `#![cfg(feature = "fix-integration-test")]` | real loopback sockets, **no external service** |

`fix_adapter.rs` covers the wiring-level guards: historical rejection for every
source factory in both poll modes, and `fix_send`'s realtime-only check at run
start. Codec / session unit tests live inline in `fix.rs`
(`encode_decode_roundtrip`, `custom_logon_fields_are_sent`,
`password_logon_sends_username_and_password`, the two `FixSender` queue cases).

`fix_integration.rs` stands up an in-process acceptor + initiator over real
loopback sockets — the port of legacy's `fix_same_process_spin` /
`fix_same_process_threaded`. `fix_same_process_spin` is the **guard for
register A7**.

```bash
cargo test --manifest-path crates/wingfoil/Cargo.toml --features fix --test fix_adapter
cargo test --manifest-path crates/wingfoil/Cargo.toml --features fix-integration-test -- --test-threads=1
```

**Workflow:** `.github/workflows/fix-next-integration.yml` (in
`integration-tests.yml`), Rust leg + `pytest -m requires_fix` Python leg.

## Example

`examples/fix_adapter.rs`, `required-features = ["fix"]`.

## Python

`wingfoil-python` feature `fix = ["wingfoil/fix", "_common"]`. **In
`all-adapters` and in the wheel** (pure Rust).

- **A mix of macro and hand-written** — the worked example of that rule.
  `fix_connect`, `fix_accept`, `fix_send` are `#[pyadapter]`; only
  `fix_connect_tls` is hand-written, because it returns the engine's
  `FixConnection` handle. Do **not** hand-write a whole module because one
  entry point needs it.
- The hand-written fn erases at the same seams the macro's source arm emits:
  `graph: PyRef<'_, Graph>` → `graph.object().builder()` →
  `erase_burst_source::<T>` / `erase_source::<T>`, with the resulting
  `PyStream`s stored in a `#[pyclass] PyFixConnection` and handed out through
  `#[getter]`s.
- `FixConnection.send` marshals **synchronously**, which makes it a free unit
  harness for the dict→`FixMessage` path — no run, no service. Reach for that
  when a sink's own errors are unreachable (the adapter connects at `start()`,
  before the first cycle).
- Tests: `tests/test_fix.py` — service-free group by default;
  `@pytest.mark.requires_fix` marks the loopback round trips, which need a
  **live wall clock** rather than an external service. The marker docstring
  says so, since `requires_x` otherwise implies a service.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test --manifest-path crates/wingfoil/Cargo.toml --features fix
cargo test --manifest-path crates/wingfoil/Cargo.toml --features fix-integration-test -- --test-threads=1
```
