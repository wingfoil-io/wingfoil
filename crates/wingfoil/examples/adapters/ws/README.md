# ws Adapter Example (wingfoil)

A reconnecting WebSocket client feeding a graph — the transport half of a
streaming venue adapter.

**No service to start.** The example runs its own synthetic "venue" in-process:
a WebSocket server that waits for a subscription, publishes three quotes, then
**deliberately hangs up**. That drop is the point of the example. The graph
contains no reconnect logic of its own, yet quotes keep arriving, because
`ws_sub`/`ws_connect` own the connect → resubscribe → back off → connect loop.

## Prerequisites

None.

## Run

```sh
cargo run -p wingfoil --example ws_adapter --features ws
```

## Code

`ws_connect` returns three things for one connection: the frames, the
connection state as an on-graph stream, and an outbound sender (unused here).

```rust
let connection = ws_connect(
    &g,
    RunMode::RealTime,
    WsConfig::new(format!("ws://127.0.0.1:{port}/stream"))
        .subscribe(r#"{"op":"subscribe","args":["quotes.BTC-USD"]}"#)
        .backoff(WsBackoff {
            initial: Duration::from_millis(100),
            max: Duration::from_millis(500),
            ..WsBackoff::default()
        }),
)?;
```

The subscription is part of the *config*, not something sent once at startup —
which is what makes it go out again on every reconnect.

## Output

```text
venue: received {"op":"subscribe","args":["quotes.BTC-USD"]}
-- connected, subscription sent
quote: {"symbol":"BTC-USD","bid":50000.0,"ask":50000.5}
quote: {"symbol":"BTC-USD","bid":50001.0,"ask":50001.5}
quote: {"symbol":"BTC-USD","bid":50002.0,"ask":50002.5}
-- venue hung up; reconnecting (attempt 1)
venue: received {"op":"subscribe","args":["quotes.BTC-USD"]}
-- connected, subscription sent
quote: {"symbol":"BTC-USD","bid":50003.0,"ask":50003.5}
quote: {"symbol":"BTC-USD","bid":50004.0,"ask":50004.5}
quote: {"symbol":"BTC-USD","bid":50005.0,"ask":50005.5}
-- venue hung up; reconnecting (attempt 1)
venue: received {"op":"subscribe","args":["quotes.BTC-USD"]}
-- connected, subscription sent
quote: {"symbol":"BTC-USD","bid":50006.0,"ask":50006.5}
quote: {"symbol":"BTC-USD","bid":50007.0,"ask":50007.5}
quote: {"symbol":"BTC-USD","bid":50008.0,"ask":50008.5}
-- done
```

Two connection drops, two automatic reconnects, the subscription re-sent each
time, and an unbroken quote sequence (`50000` … `50008`) across all three
connections. `attempt 1` each time because a *successful* connect resets the
backoff — the counter tracks consecutive failures, not lifetime reconnects.

## Writing a real venue adapter

The difference is only the URL, the subscription payload, and a parsing stage.
Frames arrive as `Burst<WsMessage>`; parse each into the venue-neutral types in
[`adapters::market`](../../../src/adapters/market/CLAUDE.md) and the rest of the
graph never learns which venue it came from.

`wss://` needs the `ws-tls` feature — every real venue will.
