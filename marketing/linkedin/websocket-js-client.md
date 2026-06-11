# LinkedIn — WebSocket adapter and JS client

## Post

The Wingfoil WebSocket adapter and its JavaScript client have been quietly maturing, so a quick rundown.

On the Rust side, the `web` adapter spins up an HTTP + WebSocket server alongside your graph. `web_pub(topic)` turns a stream into something browsers can subscribe to; `web_sub(topic)` does the reverse, surfacing incoming browser frames as a stream of bursts.

The part that's been most useful is the client. We compile the wire types from Rust to WebAssembly and ship them as `@wingfoil/client`, which means the browser decodes the same struct definitions the server encoded. No hand-written TypeScript schemas, no drift when a field gets added.

There are thin reactive bindings for Solid, Svelte and Vue 3 — `useTopic(client, "price")` and the framework re-renders when a value arrives. Bincode is the default on the wire; you can switch to JSON if you want to read frames in devtools. TLS is behind a feature flag.

We mostly built it for streaming live trading state into dashboards, but it's not specific to that. Anything kHz-rate that needs to land in a browser without DOM thrash.

## First comment

- Web adapter source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil/src/adapters/web
- WASM client (Rust): https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil-wasm
- JS package source: https://github.com/wingfoil-io/wingfoil/tree/main/wingfoil-js
- Example server: https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil/examples/web/main.rs
