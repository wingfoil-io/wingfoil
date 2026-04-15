// Round-trip test that exercises wingfoil-wasm's codec from TypeScript.
// Requires the wasm bundle at ../wasm-pkg (build with `pnpm build:wasm`).

import { describe, it, expect, beforeAll } from "vitest";

let wasm: typeof import("../wasm-pkg/wingfoil_wasm.js");

beforeAll(async () => {
  wasm = await import("../wasm-pkg/wingfoil_wasm.js");
  try {
    await wasm.default?.();
  } catch {
    // nodejs wasm-pack target initialises eagerly — ignore.
  }
});

describe("wingfoil-wasm codec", () => {
  it("round-trips a JSON envelope", () => {
    const bytes = wasm.encodeEnvelope("json", "t", 7n, new Uint8Array([1, 2, 3]));
    const env = wasm.decodeEnvelope("json", bytes) as {
      topic: string;
      timeNs: bigint;
      payload: Uint8Array;
    };
    expect(env.topic).toBe("t");
    expect(env.timeNs).toBe(7n);
    expect(Array.from(env.payload)).toEqual([1, 2, 3]);
  });

  it("round-trips a Subscribe control frame", () => {
    const bytes = wasm.encodeSubscribe("bincode", ["a", "b"]);
    const env = wasm.decodeEnvelope("bincode", bytes) as {
      topic: string;
      payload: Uint8Array;
    };
    expect(env.topic).toBe(wasm.controlTopic());
    const ctrl = wasm.decodeControl("bincode", env.payload) as {
      Subscribe?: { topics: string[] };
    };
    expect(ctrl.Subscribe?.topics).toEqual(["a", "b"]);
  });

  it("reports wire protocol version", () => {
    expect(wasm.wireVersion()).toBeGreaterThanOrEqual(1);
  });
});
