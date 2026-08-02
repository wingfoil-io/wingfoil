import { describe, expect, it } from "vitest";

import { normalizeBurst } from "../src/index.js";

describe("normalizeBurst", () => {
  it("passes an array burst through unchanged", () => {
    expect(normalizeBurst([1, 2, 3])).toEqual([1, 2, 3]);
  });

  it("keeps a single-element burst as-is", () => {
    expect(normalizeBurst([42])).toEqual([42]);
  });

  it("wraps a non-array (legacy/scalar) payload as a single-element burst", () => {
    expect(normalizeBurst(42)).toEqual([42]);
    expect(normalizeBurst({ mid: 1 })).toEqual([{ mid: 1 }]);
  });

  it("preserves an empty burst (dispatch skips it)", () => {
    expect(normalizeBurst([])).toEqual([]);
  });

  it("collapsing to the latest value takes the last element", () => {
    const b = normalizeBurst([1, 2, 3]);
    expect(b[b.length - 1]).toBe(3);
  });
});
