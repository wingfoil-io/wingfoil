import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const wasm = vi.hoisted(() => ({
  control: {} as { Hello?: { codec: string; version: number } },
  encodePayload: vi.fn(() => new Uint8Array([1, 2, 3])),
  wireVersion: vi.fn(() => 2),
}));

vi.mock("../src/wasm/wingfoil_wasm.js", () => ({
  default: () => Promise.resolve(),
  init_panic_hook: () => {},
  wireVersion: wasm.wireVersion,
  controlTopic: () => "$ctrl",
  decodeEnvelope: () => ({
    topic: "$ctrl",
    timeNs: 0n,
    payload: new Uint8Array([9]),
  }),
  encodeEnvelope: () => new Uint8Array(),
  encodeSubscribe: () => new Uint8Array(),
  encodeUnsubscribe: () => new Uint8Array(),
  encodePayload: wasm.encodePayload,
  decodePayload: () => null,
  decodeControl: () => wasm.control,
}));

import { WingfoilClient, type ConnectionState } from "../src/index.js";

type SocketHandler = (event: { data?: ArrayBuffer; code?: number; reason?: string }) => void;

class FakeSocket {
  static readonly OPEN = 1;
  static readonly instances: FakeSocket[] = [];

  readyState = 0;
  binaryType = "";
  readonly send = vi.fn();
  private readonly handlers = new Map<string, SocketHandler[]>();

  constructor(_url: string) {
    FakeSocket.instances.push(this);
  }

  addEventListener(name: string, handler: SocketHandler): void {
    const handlers = this.handlers.get(name) ?? [];
    handlers.push(handler);
    this.handlers.set(name, handlers);
  }

  emit(name: string, event: Parameters<SocketHandler>[0] = {}): void {
    for (const handler of this.handlers.get(name) ?? []) handler(event);
  }

  close(): void {}
}

async function connectedClient(): Promise<{
  client: WingfoilClient;
  socket: FakeSocket;
}> {
  const client = new WingfoilClient({ url: "ws://localhost:8080/ws" });
  await vi.waitFor(() => expect(FakeSocket.instances).toHaveLength(1));
  return { client, socket: FakeSocket.instances[0] };
}

describe("WingfoilClient.publish", () => {
  beforeEach(() => {
    FakeSocket.instances.length = 0;
    wasm.control = {};
    wasm.encodePayload.mockReset();
    wasm.encodePayload.mockReturnValue(new Uint8Array([1, 2, 3]));
    wasm.wireVersion.mockReturnValue(2);
    vi.stubGlobal("WebSocket", FakeSocket);
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("reports a drop while wasm is still booting", () => {
    const client = new WingfoilClient({ url: "ws://localhost:8080/ws" });
    expect(client.publish("orders", { id: 1 })).toBe(false);
    client.close();
  });

  it("returns true only after handing the encoded frame to an open socket", async () => {
    const { client, socket } = await connectedClient();

    expect(client.publish("orders", { id: 1 })).toBe(false);
    expect(socket.send).not.toHaveBeenCalled();

    socket.readyState = FakeSocket.OPEN;
    expect(client.publish("orders", { id: 2 })).toBe(true);
    expect(wasm.encodePayload).toHaveBeenCalledWith("json", "orders", { id: 2 });
    expect(socket.send).toHaveBeenCalledTimes(1);
    client.close();
  });

  it("returns false when encoding or sending fails", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const { client, socket } = await connectedClient();
    socket.readyState = FakeSocket.OPEN;
    wasm.encodePayload.mockImplementationOnce(() => {
      throw new Error("unencodable");
    });

    expect(client.publish("orders", BigInt(1))).toBe(false);
    expect(warn).toHaveBeenCalledWith(
      "wingfoil: publish failed on",
      "orders",
      expect.any(Error),
    );

    socket.send.mockImplementationOnce(() => {
      throw new Error("socket closed during send");
    });
    expect(client.publish("orders", { id: 2 })).toBe(false);
    expect(warn).toHaveBeenCalledTimes(2);
    client.close();
  });
});

describe("WingfoilClient Hello version handling", () => {
  beforeEach(() => {
    FakeSocket.instances.length = 0;
    wasm.control = {};
    wasm.encodePayload.mockReset();
    wasm.encodePayload.mockReturnValue(new Uint8Array());
    wasm.wireVersion.mockReturnValue(2);
    vi.stubGlobal("WebSocket", FakeSocket);
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it("surfaces a mismatch and keeps the connection open", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const { client, socket } = await connectedClient();
    const states: ConnectionState[] = [];
    client.onConnection((state) => states.push(state));
    wasm.control = { Hello: { codec: "json", version: 1 } };

    socket.emit("message", { data: new Uint8Array([0]).buffer });

    expect(error).toHaveBeenCalledTimes(1);
    expect(error.mock.calls[0][0]).toContain(
      "wire protocol version mismatch (client 2, server 1)",
    );
    expect(client.version).toBe(1);
    expect(states).toContainEqual({ kind: "open", codec: "json", version: 1 });
    client.close();
  });

  it("does not report matching versions", async () => {
    const error = vi.spyOn(console, "error").mockImplementation(() => {});
    const { client, socket } = await connectedClient();
    wasm.control = { Hello: { codec: "json", version: 2 } };

    socket.emit("message", { data: new Uint8Array([0]).buffer });

    expect(error).not.toHaveBeenCalled();
    client.close();
  });
});
