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
  codec: "json",                   // the default; the only codec a browser can use
});

client.subscribe("price", (value, timeNs) => {
  console.log(timeNs, value);
});

// Send a UI event back to the graph. `false` means it was dropped because
// the client was still booting/reconnecting or encoding/sending failed.
if (!client.publish("ui", { kind: "click", note: "hi" })) {
  // dropped -- see "Publishing is best effort" below
}
```

Start the server to match:

```rust
WebServer::bind("127.0.0.1:8080")
    .codec(CodecKind::Json)
    .start()?;
```

### Publishing is best effort

`publish()` returns `true` only after it hands the encoded frame to an open
WebSocket. It returns `false` while wasm is loading, during the initial connect
or a reconnect, and when encoding or `WebSocket.send()` fails. The Solid,
Svelte, and Vue publisher helpers return the same boolean.

Publishes are not buffered or replayed. A stale UI or order event can be more
dangerous than a visible drop after reconnect, so callers that require delivery
should check the return value and apply a bounded, domain-specific retry policy.
A drop while booting or reconnecting may be transient; retrying the same value
that JSON cannot encode will never succeed. Encoding and send failures also log
a warning, but the boolean alone does not distinguish failure causes.
Subscriptions are different: they describe desired connection state and are
therefore replayed automatically after reconnect.

### Data payloads require the JSON codec

**`subscribe` / `subscribeBurst` / `publish` only work when the server is
started with `.codec(CodecKind::Json)` and the client uses `codec: "json"`
— which is the client's default.** An explicit `codec: "bincode"` is
rejected by the `WingfoilClient` constructor, with a message that says all
of this and names both halves of the fix. The failure lands once, at the
line that chose the codec, rather than as a `console.warn` per publish and
per inbound frame forever.

The client defaults to `"json"` even though the *server* defaults to
`CodecKind::Bincode`. The envelope codec has to match the server either
way, so a browser user configures both sides whatever the default is — and
of the two matching pairs, only JSON/JSON can carry a payload. A default
the browser can never use is not a useful default.

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
connection would still connect, subscribe and receive frames. That is
precisely why the constructor refuses it: the connection looks healthy in
the network tab while every data frame fails. It is only the user
*payload*, whose type only the server knows, that bincode cannot carry to
or from a browser. A Rust or Python client, which does have the schema,
can use bincode freely.

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

Whether a slow client can hold the graph up is the **server's** decision,
and the server's default (`Delivery::Auto`) splits it by run mode. Against a
**live** graph the client is lossy and never back-pressures it: a tab that
falls behind is already showing stale data, and stalling a live system is
worse than dropping a frame. Against a **historical replay** the server paces
itself to the slowest subscriber, so the client receives the whole replay in
order — a replay has no live clock to fall behind, so dropping frames there
would just put holes in what you draw. Nothing in the client changes either
way, and the wire format is identical.

Two consequences worth knowing on the browser side: against a paced replay,
*not reading* (a backgrounded tab that stops draining its socket) holds the
server's graph up rather than losing frames — until the server's
`lossless_stall_timeout` (30 s by default) decides the tab is gone. At that
point the server **closes the connection abruptly** — the writer task is
aborted without a WebSocket Close frame, so the client sees an *abnormal* drop
(1006) and its normal reconnect applies; it does not sit on a live-looking
socket that will never deliver another frame or a `Complete`. The abruptness is
load-bearing: a clean close (1000/1001) means "session done" to this client and
stops reconnection (see "No reconnect loop" above), so a server sending a
proper Close frame on withdrawal would strand exactly the recoverable clients.
Note that a reconnecting client rejoins a replay already in progress and has
missed whatever went out while it was away — losslessness is a property of a subscription, not of the
topic. A server built with `Delivery::Lossy` restores the always-drop behaviour
in both modes. See
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
pnpm test             # vitest suite (tests/) — needs build:wasm first
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
`$ctrl` frames, whose shapes both sides know, work under either, but
because a payload does not, `WingfoilClient` accepts only `codec: "json"`
(its default) and rejects `"bincode"` at construction.

The control plane (topic `$ctrl`) carries `Hello` on connect, `Subscribe`
/ `Unsubscribe` from the client, and `Complete { topic }` from the server
when a publish topic's stream ends (wire protocol version 2). `Complete`
was appended to the message enum, so a version-1 server that never sends
it stays compatible — the client simply never fires `onComplete`. When the
server's Hello version differs from the client's `wireVersion()`, the client
logs one explicit error for that connection but stays connected because wire
versions can remain backward-compatible.
