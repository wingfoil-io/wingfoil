# @wingfoil/client

TypeScript / JavaScript client for the wingfoil [`web`
adapter](../crates/wingfoil/src/adapters/web). Wraps the
[`wingfoil-wasm`](../crates/wingfoil-wasm) decoder and exposes a small
framework-agnostic `WingfoilClient` plus optional reactive-framework
adapters for Solid.js, Svelte, and Vue 3.

The Rust server is the single source of truth for the wire format — the
browser imports a Rust-compiled-to-wasm codec instead of maintaining
hand-written TypeScript schemas.

## Install

Not yet published. During development, point your app at the local
package (see `vite.config.ts` for the alias pattern).

```jsonc
// package.json
{ "dependencies": { "@wingfoil/client": "^4.0.1" } }
```

## Quick start

```ts
import { WingfoilClient } from "@wingfoil/client";

const client = new WingfoilClient({
  url: "ws://localhost:8080/ws",
  codec: "json",                   // required for data payloads — see below
});

client.subscribe("price", (value, timeNs) => {
  console.log(timeNs, value);
});

// Send a UI event back to the graph:
client.publish("ui", { kind: "click", note: "hi" });
```

Start the server to match:

```rust
WebServer::bind("127.0.0.1:8080")
    .codec(CodecKind::Json)
    .start()?;
```

### Data payloads require the JSON codec

**`subscribe` / `subscribeBurst` / `publish` only work when the server is
started with `.codec(CodecKind::Json)` and the client is constructed with
`codec: "json"`.** Under the server's default `CodecKind::Bincode`,
`publish` and payload decoding throw — deliberately, and with a message
that says this.

The reason is structural, not a missing feature. bincode is schema-driven
and non-self-describing: the bytes carry no field names and no type tags,
so both encoding and decoding need the Rust type. The browser does not
have it. A JS object can only be encoded as a length-prefixed *map*, while
the server's `deserialize_struct` expects bare fields in declaration order
— the two do not line up, and the mismatch is **not** an error on the
server: it decodes silent garbage. In the other direction, a schema-less
decode of bincode bytes fails outright. So the client refuses both rather
than corrupting your data.

Connection-level frames are unaffected — the envelope and the `$ctrl`
control messages have fixed shapes known to both sides, so a bincode
connection still connects, subscribes and receives frames. It is only the
user *payload*, whose type only the server knows, that bincode cannot
carry to or from a browser. A Rust or Python client, which does have the
schema, can use bincode freely.

### Bursts

A frame's payload can be a **burst** — several values that share one
`timeNs`. A scalar payload (a number or struct) is treated as a
one-element burst; a payload that decodes to an **array** is the whole
group. (A wingfoil graph produces a group by publishing a `Stream<Vec<T>>`
— e.g. `web_sub`'s `Burst<T>` mapped to `Vec<T>`.)

`subscribe` collapses the burst to its latest value (the right default for
"show the current value"). When you must not drop same-timestamp values —
e.g. appending every point to a chart — subscribe to the whole burst:

```ts
client.subscribeBurst("price", (values, timeNs) => {
  for (const v of values) series.push(v);   // values: T[]
});
```

## Streaming historical data (backtests / slow computations)

The same client works for a graph running in historical mode
(`RunMode::HistoricalFrom`) served over a normal `WebServer::…start()` —
a backtest or slow computation streams its `web_pub` output to the browser
frame-by-frame, so you can watch a replay unfold. Two things differ from a
live feed:

- **End-of-stream.** When a historical replay reaches the end of its
  source, the server sends a `Complete` control frame. Observe it with
  `onComplete` to render "replay finished" and stop any progress UI:

  ```ts
  client.onComplete((topic) => {
    console.log(`stream ${topic} finished`);
  });
  ```

- **No reconnect loop.** A finished replay must not reconnect against a
  server that has intentionally shut down. Once the client sees a
  `Complete` frame — or the server closes with a normal code (1000 / 1001)
  — it treats the session as done and stops reconnecting, regardless of
  `reconnectMs`. Only an abnormal drop (e.g. 1006) still retries.

Streaming clients are lossy and never back-pressure the graph, so a
loss-free replay depends on the graph not outrunning the client (a
genuinely compute-bound historical run is the natural fit). See
`crates/wingfoil/examples/web` (`WINGFOIL_WEB_HISTORICAL=1`) for a runnable demo.

## Latency tracing

For UIs that drive a wingfoil server using the `Traced<T, L>` /
`latency_stages!` pattern, `@wingfoil/client/tracing` provides a
`LatencyTracker` that owns the per-tab session UUID, stamps outbound
requests with `client_seq` + `t_client_send`, filters inbound responses
to the current session, and (optionally) echoes the round-trip back so
the server can compute `rtt_total` / `wire_rtt` within a single clock
domain. The listener receives the four deltas pre-computed.

```ts
import { WingfoilClient } from "@wingfoil/client";
import { LatencyTracker } from "@wingfoil/client/tracing";

const client = new WingfoilClient({ url: "ws://localhost:8080/ws", codec: "json" });
const tracker = new LatencyTracker({
  client,
  outbound: "orders",
  inbound:  "fills",
  echo:     "latency_echo",   // omit to disable the echo leg
});

tracker.onResponse<FillFrame>(({ payload, rttNs, serverResidentNs, wireRttNs, stamps }) => {
  console.log(payload.client_seq, rttNs, serverResidentNs, wireRttNs);
});

// session, client_seq, and t_client_send are stamped by the tracker.
tracker.send({ side: 0, qty: 1 });
```

The default field names match the wingfoil convention (`session`,
`client_seq`, `t_client_send`, `t_client_recv`, `stamps`) and can be
overridden via `LatencyTrackerOptions.fields` (the same map applies to
both outbound publishes and inbound parsing). The end-to-end latency
demo at `crates/wingfoil/examples/showcase/trading_e2e/static/app.js` is the canonical
example.

Requires the server to use `CodecKind::Json` — as every data payload does
([above](#data-payloads-require-the-json-codec)). The tracker adds a
second reason of its own: it sends `session` as a JS `number[]`, which the
JSON codec round-trips as a Rust `[u8; 16]` but bincode would encode as a
length-prefixed `Vec<u8>`.

The main package also re-exports the small browser helpers the tracker
relies on, in case you need them directly: `newSessionId`,
`sessionHex`, `nowNs`.

## Reactive-framework bindings

### Solid.js

```tsx
import { useTopic, usePublisher } from "@wingfoil/client/solid";

function LivePrice({ client }) {
  const price = useTopic<PriceTick>(client, "price");
  const sendClick = usePublisher(client, "ui");
  return (
    <div>
      {price()?.mid.toFixed(4)}
      <button onClick={() => sendClick({ kind: "click", note: "" })}>go</button>
    </div>
  );
}
```

Solid's fine-grained signals are the recommended default for kHz+
streams — signal writes are cheap and paints coalesce to rAF, so
high-frequency data drives UI without per-frame DOM thrash.

`useTopic` surfaces the latest value; `useTopicBurst` surfaces the whole
same-`timeNs` burst (`Accessor<T[] | undefined>`) when you need every
value — e.g. appending each point of a historical replay to a chart.

### Svelte

```svelte
<script lang="ts">
  import { topic, publisher } from "@wingfoil/client/svelte";
  const price = topic<PriceTick>(client, "price");
  const send = publisher(client, "ui");
</script>
{#if $price}<div>{$price.mid.toFixed(4)}</div>{/if}
```

### Vue 3

```vue
<script setup lang="ts">
import { useTopic, usePublisher } from "@wingfoil/client/vue";
const price = useTopic<PriceTick>(client, "price");
const send = usePublisher(client, "ui");
</script>
<template><div>{{ price?.mid.toFixed(4) }}</div></template>
```

### Not included

Generic React bindings are intentionally *not* shipped as a first-class
target. React re-renders at kHz without manual batching will tank
frame-rate — use Solid or Svelte instead, or implement a React adapter
with `useSyncExternalStore` coalesced to `requestAnimationFrame` if you
need React.

## Development

From `js/`:

```sh
pnpm install
pnpm run build:wasm   # wasm-pack → ./src/wasm
pnpm build            # build:wasm + tsc + copy ./src/wasm to ./dist/wasm
pnpm dev              # Vite dev server for examples/solid-dashboard
pnpm run lint         # tsc --noEmit
```

Codec round-trip coverage lives in the Rust unit tests of
`wingfoil-wasm` (run with `cargo test` in that crate) and in
`wasm-pack test` for browser-target coverage.

Start the Rust example in another terminal:

```sh
cargo run --example web --features web
```

Then open <http://localhost:5173> — the Solid dashboard connects to
`ws://127.0.0.1:8080/ws` by default.

## Wire format

Every WebSocket frame is binary — either a `bincode`-serialized
[`Envelope`](../crates/wingfoil-wire-types/src/lib.rs) (the server's default) or
a JSON one (if the server was started with `.codec(CodecKind::Json)`). The
`wingfoil-wasm` decoder handles both envelope framings without any user
configuration other than the codec hint passed to `WingfoilClient`.

The payload is the stream's value serialized by the codec. A scalar is a
single value; a value that decodes to an array is surfaced as a
same-`timeNs` **burst** (the client collapses it for `subscribe` and passes
it whole to `subscribeBurst`). Client → server frames carry a single value.

**Payloads are the exception to "handles both".** A browser has no Rust
schema, and bincode needs one in both directions, so a browser client
requires the JSON codec for any payload it sends or receives —
[see above](#data-payloads-require-the-json-codec). The envelope and
`$ctrl` frames, whose shapes both sides know, work under either.

The control plane (topic `$ctrl`) carries `Hello` on connect, `Subscribe`
/ `Unsubscribe` from the client, and `Complete { topic }` from the server
when a publish topic's stream ends (wire protocol version 2). `Complete`
was appended to the message enum, so a version-1 server that never sends
it stays compatible — the client simply never fires `onComplete`.
