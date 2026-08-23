import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The client imports the generated wasm module at load time. These tests are
// about the codec decision the constructor makes *before* any of it is used,
// so stand the module in rather than depending on a real wasm build.
vi.mock("../src/wasm/wingfoil_wasm.js", () => ({
  default: () => Promise.resolve(),
  init_panic_hook: () => {},
  wireVersion: () => 1,
  controlTopic: () => "$ctrl",
  decodeEnvelope: () => ({ topic: "", timeNs: 0n, payload: new Uint8Array() }),
  encodeEnvelope: () => new Uint8Array(),
  encodeSubscribe: () => new Uint8Array(),
  encodeUnsubscribe: () => new Uint8Array(),
  encodePayload: () => new Uint8Array(),
  decodePayload: () => null,
  decodeControl: () => ({}),
}));

import {
  BINCODE_PAYLOAD_UNSUPPORTED,
  WingfoilClient,
  resolveCodec,
} from "../src/index.js";

describe("resolveCodec", () => {
  it("defaults to json when no codec is given", () => {
    expect(resolveCodec(undefined)).toBe("json");
  });

  it("passes an explicit json codec through", () => {
    expect(resolveCodec("json")).toBe("json");
  });

  it("rejects an explicit bincode codec", () => {
    expect(() => resolveCodec("bincode")).toThrow(BINCODE_PAYLOAD_UNSUPPORTED);
  });

  it("names both halves of the fix in the rejection", () => {
    let message = "";
    try {
      resolveCodec("bincode");
    } catch (err) {
      message = (err as Error).message;
    }
    // The server half and the client half — a browser user configures both.
    expect(message).toContain(".codec(CodecKind::Json)");
    expect(message).toContain('codec: "json"');
    // And it points at the line that chose the codec.
    expect(message).toContain("WingfoilClient:");
  });
});

// A stand-in for the browser WebSocket: constructing a client connects, and
// nothing here should touch the network.
class FakeSocket {
  static readonly OPEN = 1;
  readyState = 0;
  binaryType = "";
  addEventListener(): void {}
  send(): void {}
  close(): void {}
}

describe("WingfoilClient codec rejection at construction", () => {
  beforeEach(() => {
    vi.stubGlobal("WebSocket", FakeSocket);
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("throws from the constructor on an explicit bincode codec", () => {
    expect(
      () => new WingfoilClient({ url: "ws://localhost:8080/ws", codec: "bincode" }),
    ).toThrow(BINCODE_PAYLOAD_UNSUPPORTED);
  });

  it("surfaces the problem once, not once per frame", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    expect(
      () => new WingfoilClient({ url: "ws://localhost:8080/ws", codec: "bincode" }),
    ).toThrow();
    // No socket, no warnings — the failure landed at construction.
    expect(warn).not.toHaveBeenCalled();
    warn.mockRestore();
  });

  it("constructs with the default codec", async () => {
    const client = new WingfoilClient({ url: "ws://localhost:8080/ws" });
    expect(client).toBeInstanceOf(WingfoilClient);
    client.close();
    // Let the async boot settle against the closed client before unstubbing.
    await new Promise((resolve) => setTimeout(resolve, 0));
  });

  it("constructs with an explicit json codec", async () => {
    const client = new WingfoilClient({
      url: "ws://localhost:8080/ws",
      codec: "json",
    });
    expect(client).toBeInstanceOf(WingfoilClient);
    client.close();
    await new Promise((resolve) => setTimeout(resolve, 0));
  });
});
