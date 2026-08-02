// Svelte bindings for @wingfoil/client.
//
// Exposes a `topic(client, name)` factory returning a Svelte `Readable`
// store that yields the latest decoded value for the topic. Unsubscribes
// automatically when the last Svelte subscriber disconnects.

import { readable, type Readable } from "svelte/store";
import type { WingfoilClient } from "./index.js";

/**
 * Create a Svelte readable store backed by a topic subscription.
 *
 * @example
 * ```svelte
 * <script lang="ts">
 *   import { topic } from "@wingfoil/client/svelte";
 *   const price = topic<PriceTick>(client, "price");
 * </script>
 * {#if $price}{$price.mid}{/if}
 * ```
 */
export function topic<T>(
  client: WingfoilClient,
  name: string,
): Readable<T | undefined> {
  return readable<T | undefined>(undefined, (set) => {
    const unsubscribe = client.subscribe(name, (v) => set(v as T));
    return unsubscribe;
  });
}

/** Helper to publish from Svelte components. */
export function publisher<T>(client: WingfoilClient, name: string): (value: T) => void {
  return (value) => client.publish(name, value);
}
