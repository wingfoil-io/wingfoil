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
pnpm test             # vitest unit tests
```

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
