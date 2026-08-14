# ws adapter (wingfoil)

`src/adapters/ws.rs`, features `ws` (+ `ws-tls` for `wss://`). **No legacy
twin** — wingfoil-only, like `lines` and `market`.

A reconnecting WebSocket **client** transport. Not to be confused with
[`web`](../web/CLAUDE.md), which is the WebSocket *server* a browser connects
to; the two share `tokio-tungstenite` and nothing else.

## What it is for

Every streaming venue adapter (Binance, Coinbase, Bybit, …) needs the same
loop: connect, send a subscribe payload, read frames until the venue drops you,
back off, connect again, **send the subscribe payload again**. This module owns
that loop so each out-of-tree venue crate is left with only its own parsing.

It is deliberately payload-agnostic — it yields raw [`WsMessage`] frames and
knows nothing about JSON, venue envelopes or `market` types. It is the
transport half of the story whose vocabulary half is
[`market`](../market/CLAUDE.md).

## Entry points

| Item | Kind | Notes |
|---|---|---|
| `ws_sub(g, run_mode, cfg)` | source | `Result<Stream<Burst<WsMessage>>>` — frames only |
| `ws_connect(g, run_mode, cfg)` | source | `Result<WsConnection>` — `.messages`, `.status`, `.sender` |
| `WsConfig::new(url)` + `.subscribe()` / `.backoff()` / `.idle_timeout()` / `.ping_interval()` / `.buffer_size()` | config | chainable; `From<&str>`/`From<String>` for the bare-URL case |
| `WsConfig::redacted()` | config | **the only form allowed in an error message** |
| `WsBackoff` | config | exponential + equal jitter; `max_attempts: None` retries forever |
| `WsStatus` | value | `Disconnected` (default) / `Connected` / `Reconnecting { attempt }` / `Failed` |
| `WsSender::send()` | handle | non-blocking queue push; safe from a cycle or any thread |
| `WsSinkOps::ws_send(&sender)` | sink trait | on `Stream<Burst<WsMessage>>` **and** `Stream<WsMessage>` |
| `WsItem` / `WsItemOps` | plumbing | the multiplexed item and its demux; public only because the ops are |

## What to know before changing it

- **Realtime only.** Both factories reject `RunMode::HistoricalFrom` at wiring
  (register B2). A live socket has no historical timeline; record frames and
  replay the recording for a backtest.
- **Wiring validates, `start()` connects.** The URL scheme, the TLS feature and
  the run mode are checked at wiring and return `Err`. A *connection* failure is
  not a wiring error — it is retried per `WsBackoff`.
- **A disconnect is not an error.** It is a `WsStatus` transition and a
  reconnect. The run aborts only when `WsBackoff::max_attempts` is set and
  exhausted; that is the one way this source ever ends. The default `None`
  retries forever, which is right for production and wrong for a test — every
  test here sets `max_attempts` or bounds the run.
- **An attempt succeeds on the first frame, not on the handshake.** The retry
  counter is cleared when the peer sends a `Text`/`Binary`/`Ping`/`Pong` —
  never on `connect_async` returning `Ok`, and never on a `Close`. A venue that
  accepts the upgrade and then closes (rejected auth, malformed subscription,
  IP ban) otherwise resets the counter every cycle, so `max_attempts` can never
  be reached and the loop hammers the venue at `initial` forever. A
  refused-port test cannot catch this, because there the connect never
  succeeds; `a_flapping_connection_still_exhausts_the_backoff` is the guard.
- **Subscriptions are config, not a startup action.** `WsConfig::subscriptions`
  is re-sent after *every* connect. This is the whole point of the adapter: a
  venue that drops you leaves a live socket carrying no subscriptions, and the
  graph then sits silent looking perfectly healthy. The
  `subscriptions_are_resent_after_a_reconnect` test is the guard.
- **Subscribe happens before `Connected` is emitted**, so a downstream reacting
  to `Connected` cannot observe an open-but-unsubscribed socket.
- **Status is multiplexed in band** with the frames over one channel and split
  on the graph (`WsItem` → `ws_messages` / `ws_status`), so `Connected` is
  strictly ordered before the frames that followed it. A separate source could
  not promise that. Frames keep their burst; status does not — it is
  level-triggered state, so the op takes the last transition in the cycle.
- **The idle timeout is not optional in practice.** Venues routinely stop
  sending without closing the TCP connection, and no socket-level error will
  ever tell you. `idle_timeout` is the only thing that notices.
- **A `split()` socket does not auto-answer pings.** The pump replies to `Ping`
  with `Pong` explicitly; removing that silently breaks liveness on venues that
  reap unresponsive clients.
- **The select loop's outbound branch is guarded by `outbound_open`.** A closed
  `mpsc::Receiver::recv()` is *instantly ready* and would win the race against
  the socket read on every turn, starving the connection of frames. `ws_sub`
  drops its sender immediately, so this is the common path, not an edge case.
  It cost a full test-suite failure to find; do not replace the guard with a
  `pending()` await inside the branch handler (that wedges the loop instead).
- **Credentials never reach error context.** Venue URLs carry `?api_key=` and
  `wss://user:pass@host` routinely. Every error site formats
  `WsConfig::redacted()`; `wiring_errors_never_leak_credentials` pins it.
- **Jitter has no `rand` dependency** — a xorshift over a wall-clock read. Equal
  jitter (`[delay/2, delay]`; *full* jitter would be `[0, delay]`) matters
  because a venue restart disconnects every client at the same instant, and an
  unjittered fleet reconnects in lockstep forever.
- **`ws-tls` installs the ring crypto provider** on first use. rustls 0.23
  panics at connect time if a process links more than one provider and none was
  chosen — which happens as soon as a binary also pulls in `fix` or `web-tls`.
- `produce_async` ⇒ the `block_on` footgun (A5a): build, run and drop the graph
  from a **non-async** thread.

## Deviations from convention

The canonical list is the `# Deviations` discussion in the module `//!` header.
Two worth flagging here because they depart from `/new-adapter`:

1. **`ws_connect` returns a `WsConnection` struct, not a `*_with_status`
   tuple** (skill step 8a). Three parallel outputs (frames, status, sender)
   make a 3-tuple unreadable, and the primary `ws_sub` signature is left
   untouched exactly as the step requires.
2. **`WsItem` and `WsItemOps` are public** although they are plumbing. The
   `#[op(build = …)]` impls are public, so their input type cannot be private.
   Neither factory returns a `WsItem`.

## Tests

Tier 1 only — the loopback server is started by the test file.

- `src/adapters/ws.rs` `mod tests` — URL redaction (userinfo, secret query
  keys, case-insensitivity, an `@` in the path) and the backoff schedule
  (growth, cap, `u32::MAX` saturation, jitter bounds and variation, a
  multiplier below 1.0).
- `tests/ws_adapter.rs`, `#![cfg(feature = "ws")]` — wiring rejections
  (historical, non-WS scheme, `wss://` without `ws-tls`, credential leaks),
  frame ordering, subscribe-on-connect, **resubscribe-after-reconnect**, the
  idle-timeout reconnect, status transitions, backoff exhaustion aborting the
  run (both for a refused port and for **a handshake that completes and then
  closes**), and the outbound sender (including frames queued before connect).

These assert **values, not tick times**: a realtime-only source stamps
wall-clock reads, so there is nothing deterministic to assert about its
timestamps. The historical-determinism convention applies to replay sources.

```bash
cargo test -p wingfoil --features ws --test ws_adapter
cargo test -p wingfoil --features ws --lib adapters::ws
```

**Workflow:** none of its own — tier 1 runs in `rust-test.yml`'s `test` job
with every other adapter. There is no `ws-integration-test` feature because
there is no service to stand up.

## Example

`examples/adapters/ws/main.rs` → example `ws_adapter`,
`required-features = ["ws"]`. Self-contained: it runs a synthetic venue that
hangs up after three quotes, so the reconnect and resubscribe are visible in
the output. The README's sample output is a real run.

## Python

`wingfoil-python` feature `ws = ["wingfoil/ws-tls", "_common"]` — note it turns
on **`ws-tls`**, not plain `ws`, for the same reason `web` turns on `web-tls`:
rustls is pure Rust, so it costs the wheel only build time, and every real venue
is `wss://`. **In `all-adapters` and in the wheel.**

- **A mixed binding**, following `fix`. `ws_sub` is `#[pyadapter]`-generated;
  `WsConnection` is a hand-written `#[pyclass]` (`.messages`, `.status`,
  `.send(msg)`, `.send_stream(stream)`) because `ws_connect` returns three
  things and the macro emits one function with one return type.
- Value edge: `Text` ↔ Python `str`, `Binary` ↔ Python `bytes` — exact in both
  directions, unlike `web`'s `bytes` → list-of-ints hop. Anything else raises,
  naming both accepted types.
- `WsStatus` erases to a **`dict`** (`{"state": …}`, plus `"attempt"` when
  reconnecting), not a string as `aeron`'s status does. A string would drop
  `Reconnecting`'s retry count, which is the reason to watch the stream at all.
- Tests: `crates/wingfoil-python/tests/test_ws.py` — **one group, no marker,
  no service, run by default** in `python-test.yml`. The round trips use a
  ~70-line stdlib WebSocket server in the test file. That is the mirror image
  of `web`: testing a *server* binding needs a real WebSocket client (hence its
  `requires_web` marker and the `websockets` package), while testing a *client*
  binding only needs a server, which is small enough to hand-roll. There is
  therefore **no `ws-integration.yml`** and no marked tier.
- Rust-side marshaling tests live in `src/adapters/ws.rs`'s `mod tests` and run
  in `python-test.yml` via `--features all-adapters`.

```bash
cd crates/wingfoil-python && maturin develop -F extension-module,ws && pytest -q tests/test_ws.py
```

## Not done yet

- **No `wss://` test.** Tier 1 is plaintext loopback. The TLS path is exercised
  only by compiling under `ws-tls`; a self-signed fixture like `web-tls`'s
  (`rcgen`) would close that gap.
- **No automatic re-request hook after a gap.** A venue adapter pairing this
  with `market`'s `OrderBook` still has to re-request its snapshot on
  `WsStatus::Connected`; the transport cannot know what a snapshot is.
