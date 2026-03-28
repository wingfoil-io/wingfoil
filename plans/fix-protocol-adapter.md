# FIX Protocol Adapter — Implementation Plan

## Summary

Add a FIX (Financial Information eXchange) protocol adapter to wingfoil, following the patterns established by the ZMQ and WebSocket adapters.

## Motivation

FIX is the standard protocol for electronic trading (order routing, market data, execution reports). As a stream processing library targeting HFT/trading use cases, first-class FIX support is a natural fit.

## API

```rust
pub enum FixPollMode {
    /// Graph spin loop drives polling — no thread, lowest latency
    AlwaysSpin,
    /// Background thread + channel — shares CPU with other work
    Threaded,
}

pub fn fix_connect(
    host: &str,
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    mode: FixPollMode,
) -> (Rc<dyn Stream<Burst<FixMessage>>>, Rc<dyn Stream<FixSessionStatus>>)
```

## Core Types

- `FixMessage` — parsed FIX message with tag/value fields, `msg_type`, seq num, `sending_time: NanoTime`
- `FixSessionStatus` — `Disconnected | LoggingIn | LoggedIn | LoggedOut | Error(String)`

## Poll Strategies

| Mode | Mechanism | Latency | CPU |
|---|---|---|---|
| `AlwaysSpin` | Non-blocking socket polled by graph loop via `state.always_callback()` | ~1–5µs | Full spin (single thread) |
| `Threaded` | Background thread + `ChannelSender` via `ReceiverStream` | ~10–100µs | Dedicated thread |

Both modes return identical output types — the choice is purely an operational concern.

## FIX Session Management

A shared `FixSession` struct (used by both poll modes) handles:
- Logon (MsgType=A) on `start()`
- Heartbeat (MsgType=0) / TestRequest (MsgType=1) responses
- Sequence number tracking
- Logout (MsgType=5) on `stop()`

## AlwaysSpin Mode — `FixSpinSourceNode`

Implements `MutableNode` as a root node (no active upstreams):

```rust
fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
    state.always_callback();               // spin the graph
    let mut sock = TcpStream::connect(...)?;
    sock.set_nonblocking(true)?;
    self.socket = Some(sock);
    self.session.send_logon(&mut sock)?;
    Ok(())
}

fn cycle(&mut self, _: &mut GraphState) -> anyhow::Result<bool> {
    self.value.clear();
    match self.socket.as_mut().unwrap().read(&mut self.buf) {
        Ok(n)  => { /* parse FIX frames → push to self.value */ Ok(true) }
        Err(e) if e.kind() == WouldBlock => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn upstreams(&self) -> UpStreams {
    UpStreams::default()  // root node — like always()
}
```

## Threaded Mode — `FixThreadedSourceNode`

Wraps `ReceiverStream` with a blocking-read background thread:

```rust
pub fn fix_connect_threaded(...) -> ... {
    let receiver = FixReceiver { host, port, sender_comp_id, target_comp_id };
    let events: Rc<dyn Stream<Burst<FixEvent>>> =
        ReceiverStream::new(move |s| receiver.run(s), true).into_stream();
    // split into (data, status) via MapFilterStream
}
```

The receiver thread loop:
1. Connects via blocking `TcpStream`
2. Sends Logon, enters read loop
3. Pushes `Message::RealtimeValue(FixEvent::Data(...))` or `FixEvent::Status(...)` via `ChannelSender`
4. On disconnect sends `Message::EndOfStream`

## Acceptor Node — `FixAcceptorNode`

A minimal server-side node that binds a `TcpListener` and handles the acceptor role of the FIX session. Primarily intended for in-process integration testing but usable as a real acceptor.

```rust
pub fn fix_accept(
    port: u16,
    sender_comp_id: &str,
    target_comp_id: &str,
    mode: FixPollMode,
) -> (Rc<dyn Stream<Burst<FixMessage>>>, Rc<dyn Stream<FixSessionStatus>>)
```

Behaviour:
- Binds `TcpListener` on `start()` (use port `0` to let the OS assign a free port; call `local_addr()` to discover it)
- Accepts one connection (single-session); accepts `WouldBlock` on the listener in `AlwaysSpin` mode
- Reuses `FixSession` for the acceptor role: responds to Logon with Logon, handles Heartbeat/TestRequest, sends Logout on `stop()`
- Same dual poll modes as the initiator (`AlwaysSpin` / `Threaded`)

## Sink Node — `FixSenderNode`

Implements `MutableNode`, reads from upstream `Stream<FixMessage>`, serializes to FIX wire format, writes to TCP socket.

```rust
fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
    // establish connection, send Logon
}

fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
    let msg = self.src.peek_value();
    let wire = self.session.serialize(&msg, state.time())?;
    self.socket.write_all(&wire)?;
    Ok(true)
}

fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
    self.session.send_logout(&mut self.socket)?;
    Ok(())
}
```

## Ergonomic Operator Trait

```rust
pub trait FixOperators<T: Into<FixMessage> + Element> {
    fn fix_send(self: &Rc<Self>, host: &str, port: u16,
                sender_comp_id: &str, target_comp_id: &str) -> Rc<dyn Node>;
}
```

## Implementation Steps

1. **`Cargo.toml`** — add `fix` feature flag and `fefix` dependency; add `fix` to `full` feature set
2. **`src/adapters/fix.rs`** — implement:
   - `FixMessage`, `FixSessionStatus`, `FixEvent` types
   - `FixSession` shared session handler (shared by initiator and acceptor roles)
   - `FixSpinSourceNode` (AlwaysSpin initiator path)
   - `FixThreadedSourceNode` wrapping `ReceiverStream` (Threaded initiator path)
   - `FixAcceptorNode` — binds `TcpListener`, handles acceptor session role
   - `FixSenderNode` sink
   - `fix_connect()` factory dispatching on `FixPollMode`
   - `fix_accept()` factory dispatching on `FixPollMode`
   - `FixOperators` trait extension
3. **`src/adapters/mod.rs`** — register behind `#[cfg(feature = "fix")]`
4. **`examples/messaging/fix_client.rs`** — connect, login, receive and print messages
5. **Tests**:
   - Unit tests: parse/serialize round-trip with mock `FixMessage` iterators (`RunMode::HistoricalFrom`)
   - Integration test (`fix_same_process`): `fix_accept(0, ...)` + `fix_connect("127.0.0.1", port, ...)` in one `Graph`, `RunMode::RealTime` — verifies full Logon → Heartbeat → Logout handshake and data flow in-process, following the `zmq_same_thread` pattern

## Scope

- FIX 4.2 / 4.4 / 5.0 via `fefix`
- Plain TCP transport (no TLS in initial impl)
- RealTime mode only

## Key References

- ZMQ adapter pattern: `wingfoil/src/adapters/zmq.rs`
- Always node: `wingfoil/src/nodes/always.rs`
- ReceiverStream: `wingfoil/src/nodes/receiver.rs`
- ChannelSender: `wingfoil/src/channel/kanal_chan.rs`
