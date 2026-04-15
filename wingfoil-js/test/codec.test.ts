// Round-trip test that exercises wingfoil-wasm's codec from TypeScript.
// Requires the wasm bundle at ../wasm-pkg (build with `pnpm build:wasm`).

import { describe, it, expect, beforeAll } from "vitest";

let wasm: typeof import("../wasm-pkg/wingfoil_wasm.js");

beforeAll(async () => {
  wasm = await import("../wasm-pkg/wingfoil_wasm.js");
  // In node, wasm-bindgen's `init` expects a URL or buffer. For the
  // node target specifically, wasm-pack's `nodejs` target would be
  // simpler; for the web target we skip if not available.
  try {
    await wasm.default?.();
  } catch {
    // In the nodejs wasm-pack target the module is already initialised.
  }
});

describe("wingfoil-wasm codec", () => {
  it("round-trips a JSON envelope", () => {
    const bytes = wasm.encode_envelope("json", "t", 7n, new Uint8Array([1, 2, 3]));
    const env = wasm.decode_envelope("json", bytes) as {
      topic: string;
      timeNs: bigint;
      payload: Uint8Array;
    };
    expect(env.topic).toBe("t");
    expect(env.timeNs).toBe(7n);
    expect(Array.from(env.payload)).toEqual([1, 2, 3]);
  });

  it("round-trips a Subscribe control frame", () => {
    const bytes = wasm.encode_subscribe("bincode", ["a", "b"]);
    const env = wasm.decode_envelope("bincode", bytes) as {
      topic: string;
      payload: Uint8Array;
    };
    expect(env.topic).toBe(wasm.control_topic());
    const ctrl = wasm.decode_control("bincode", env.payload) as {
      Subscribe?: { topics: string[] };
    };
    expect(ctrl.Subscribe?.topics).toEqual(["a", "b"]);
  });

  it("reports wire protocol version", () => {
    expect(wasm.wire_version()).toBeGreaterThanOrEqual(1);
  });
});
