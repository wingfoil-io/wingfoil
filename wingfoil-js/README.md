# @wingfoil/client

TypeScript / JavaScript client for the wingfoil [`web`
adapter](../wingfoil/src/adapters/web). Wraps the
[`wingfoil-wasm`](../wingfoil-wasm) decoder and exposes a small
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
  url: "ws://localhost:8080/ws",   // bincode is the default
});

client.subscribe("price", (value, timeNs) => {
  console.log(timeNs, value);
});

// Send a UI event back to the graph:
client.publish("ui", { kind: "click", note: "hi" });
```

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
demo at `wingfoil/examples/latency_e2e/static/app.js` is the canonical
example.

Requires the server to use `CodecKind::Json`: the tracker sends
`session` as a JS `number[]`, which the JSON codec round-trips as
`[u8; 16]` but the bincode codec encodes as a length-prefixed `Vec<u8>`.

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

Build the wasm bundle first:

```sh
cd wingfoil-wasm
wasm-pack build --target web --release --out-dir ../wingfoil-js/wasm-pkg
```

Then, from `wingfoil-js/`:

```sh
pnpm install
pnpm build            # tsc → ./dist
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
[`Envelope`](../wingfoil-wire-types/src/lib.rs) (default) or a JSON one
(if the server was started with `.codec(CodecKind::Json)`). The
`wingfoil-wasm` decoder handles both without any user configuration
other than the codec hint passed to `WingfoilClient`.
