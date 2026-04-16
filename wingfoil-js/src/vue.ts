// Vue 3 bindings for @wingfoil/client.
//
// Exposes `useTopic(client, name)` returning a `Ref<T | undefined>` that
// updates with every frame. Uses `shallowRef` because the payload values
// are immutable snapshots — we never mutate in place, so deep reactivity
// is wasted work.

import { onUnmounted, shallowRef, type Ref } from "vue";
import type { WingfoilClient } from "./index.js";

/**
 * Subscribe to `name` on `client` and return a Vue ref of the latest
 * decoded value.
 */
export function useTopic<T>(
  client: WingfoilClient,
  name: string,
): Ref<T | undefined> {
  const r = shallowRef<T | undefined>(undefined);
  const unsubscribe = client.subscribe(name, (v) => {
    r.value = v as T;
  });
  onUnmounted(unsubscribe);
  return r;
}

/** Helper to publish from Vue components. */
export function usePublisher<T>(client: WingfoilClient, name: string): (value: T) => void {
  return (value) => client.publish(name, value);
}
