import { describe, expect, it } from "vitest";

import { shouldReconnect } from "../src/index.js";

const base = {
  userClosed: false,
  streamFinished: false,
  closeCode: 1006, // abnormal drop
  reconnectMs: 1000,
};

describe("shouldReconnect", () => {
  it("reconnects on an abnormal drop when enabled", () => {
    expect(shouldReconnect(base)).toBe(true);
  });

  it("does not reconnect after the user closes the client", () => {
    expect(shouldReconnect({ ...base, userClosed: true })).toBe(false);
  });

  it("does not reconnect once the stream has finished (Complete received)", () => {
    expect(shouldReconnect({ ...base, streamFinished: true })).toBe(false);
  });

  it("does not reconnect when reconnect is disabled", () => {
    expect(shouldReconnect({ ...base, reconnectMs: 0 })).toBe(false);
  });

  it.each([1000, 1001])(
    "does not reconnect on a clean server close code %i",
    (closeCode) => {
      expect(shouldReconnect({ ...base, closeCode })).toBe(false);
    },
  );

  it("reconnects on other abnormal codes", () => {
    expect(shouldReconnect({ ...base, closeCode: 1011 })).toBe(true);
  });
});
