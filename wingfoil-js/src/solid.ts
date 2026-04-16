// Solid.js bindings for @wingfoil/client.
//
// Exposes `useTopic(client, topic)` returning a fine-grained
// `Accessor<T | undefined>` signal that updates with every frame. Solid's
// reactivity is a great fit for kHz streams — signal writes are cheap and
// renderers coalesce paints to rAF, so high-frequency data can drive UI
// without per-frame DOM thrash.

import { createSignal, onCleanup, type Accessor } from "solid-js";
import type { WingfoilClient } from "./index.js";

/**
 * Subscribe to `topic` on `client` and surface the latest decoded value
 * as a Solid signal. Unsubscribes automatically on component cleanup.
 *
 * @example
 * ```tsx
 * const price = useTopic<PriceTick>(client, "price");
 * return <div>{price()?.mid.toFixed(4)}</div>;
 * ```
 */
export function useTopic<T>(
  client: WingfoilClient,
  topic: string,
): Accessor<T | undefined> {
  const [value, setValue] = createSignal<T | undefined>(undefined);
  const unsubscribe = client.subscribe(topic, (v) => {
    setValue(() => v as T);
  });
  onCleanup(unsubscribe);
  return value;
}

/**
 * A small publish helper. Returns a function that, given a value, sends
 * it to `topic`. Usage in an event handler:
 *
 * ```tsx
 * const sendClick = usePublisher(client, "ui");
 * return <button onClick={() => sendClick({ kind: "click", note: "hi" })}>hi</button>;
 * ```
 */
export function usePublisher<T>(
  client: WingfoilClient,
  topic: string,
): (value: T) => void {
  return (value) => client.publish(topic, value);
}
