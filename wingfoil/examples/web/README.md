# web Adapter Example

Streams a synthetic mid-price to any connected browser over WebSocket and
receives UI events back into the graph. Demonstrates both directions of the
`web` adapter — `web_pub` (outbound) and `web_sub` (inbound).

## Run

```sh
cargo run --example web --features web
```

The server binds to `127.0.0.1:8080` by default. Override with:

```sh
WINGFOIL_WEB_ADDR=0.0.0.0:9000 cargo run --example web --features web
```

## Browser client

The sibling [`wingfoil-js`](../../../wingfoil-js) package provides a ready-made
TypeScript / Solid.js client. From the repo root:

```sh
cd wingfoil-js
pnpm install
pnpm dev    # opens http://localhost:5173 against ws://127.0.0.1:8080/ws
```

Or talk to the endpoint directly from the browser devtools:

```js
const ws = new WebSocket("ws://localhost:8080/ws");
ws.binaryType = "arraybuffer";
ws.onopen = () => {
  // Subscribe to the price topic — control frame encoded with wingfoil-wasm.
};
```

## Topics

| Topic   | Direction       | Payload type                                      |
|---------|-----------------|---------------------------------------------------|
| `price` | server → client | `{ mid: f64, count: u64 }` (published at 100 Hz)  |
| `ui`    | client → server | `{ kind: String, note: String }` (logged by graph)|

## Code

See [`main.rs`](main.rs) for the full source. Key snippets:

```rust
// Publish prices at 100 Hz.
let price_stream = ticker(Duration::from_millis(10))
    .count()
    .map(|n| PriceTick { mid: 100.0 + ((n as f64) * 0.1).sin(), count: n })
    .web_pub(&server, "price");

// Receive UI events from any connected browser.
let ui_events: Rc<dyn Stream<Burst<UiEvent>>> = web_sub(&server, "ui");
let ui_log = ui_events
    .collapse()
    .for_each(|event, time| println!("{} ui-event: {:?}", time.pretty(), event));

Graph::new(vec![price_stream, ui_log], RunMode::RealTime, RunFor::Forever).run()?;
```

## Expected output

```
wingfoil web example listening on ws://127.0.0.1:8080/ws  (codec: Bincode)
0.000_123 ui-event: UiEvent { kind: "click", note: "hello" }
...
```

After connecting the browser client, Ctrl-C to stop the server.
