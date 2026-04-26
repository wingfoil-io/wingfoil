// @wingfoil/client — browser client for the wingfoil `web` adapter.
//
// This module wraps the `wingfoil-wasm` decoder behind a small
// framework-agnostic `WingfoilClient` that manages a WebSocket, topic
// subscriptions, and listener dispatch. Reactive-framework adapters
// (Solid / Svelte / Vue) are in sibling files and build on this core.

import init, {
  controlTopic,
  decodeControl,
  decodeEnvelope,
  decodePayload,
  encodePayload,
  encodeSubscribe,
  encodeUnsubscribe,
  init_panic_hook,
  wireVersion,
} from "../wasm-pkg/wingfoil_wasm.js";

export type CodecKind = "bincode" | "json";

/** Envelope shape as surfaced by the wasm decoder. */
export interface Envelope {
  topic: string;
  timeNs: bigint;
  payload: Uint8Array;
}

/** Callback invoked for each decoded payload on a topic. */
export type TopicListener = (value: unknown, timeNs: bigint) => void;

/** Callback invoked when the underlying WebSocket state changes. */
export type ConnectionListener = (state: ConnectionState) => void;

export type ConnectionState =
  | { kind: "connecting" }
  | { kind: "open"; codec: CodecKind; version: number }
  | { kind: "closed"; code: number; reason: string }
  | { kind: "error"; error: Event };

export interface ClientOptions {
  /** WebSocket URL, e.g. `ws://localhost:8080/ws`. */
  url: string;
  /**
   * Wire codec the server is using. Defaults to `bincode` (the
   * server's default); swap to `json` if the server was started with
   * `.codec(CodecKind::Json)`.
   */
  codec?: CodecKind;
  /** Reconnect on close with this delay. Defaults to 1000 ms; `0` disables. */
  reconnectMs?: number;
  /** Optional WASM module URL override (advanced). */
  wasmUrl?: string | URL;
}

/**
 * A wingfoil client. Single WebSocket connection multiplexed over
 * any number of topics.
 */
export class WingfoilClient {
  private readonly opts: Required<ClientOptions>;
  private socket: WebSocket | null = null;
  private closed = false;
  private wasmReady = false;
  private readonly listeners = new Map<string, Set<TopicListener>>();
  private readonly connListeners = new Set<ConnectionListener>();
  private codecKind: CodecKind;
  private serverVersion: number | null = null;

  constructor(options: ClientOptions) {
    this.opts = {
      url: options.url,
      codec: options.codec ?? "bincode",
      reconnectMs: options.reconnectMs ?? 1000,
      wasmUrl: options.wasmUrl ?? "",
    };
    this.codecKind = this.opts.codec;
    void this.boot();
  }

  /** Close the socket and stop reconnecting. */
  close(): void {
    this.closed = true;
    if (this.socket) {
      try {
        this.socket.close();
      } catch {
        // ignore
      }
      this.socket = null;
    }
  }

  /** Subscribe to `topic`. Returns an unsubscribe function. */
  subscribe(topic: string, listener: TopicListener): () => void {
    const set = this.listeners.get(topic) ?? new Set<TopicListener>();
    const freshTopic = !this.listeners.has(topic);
    set.add(listener);
    this.listeners.set(topic, set);
    if (freshTopic) this.sendSubscribe([topic]);
    return () => this.unsubscribe(topic, listener);
  }

  /** Remove a listener; optionally also send an unsubscribe frame if empty. */
  unsubscribe(topic: string, listener: TopicListener): void {
    const set = this.listeners.get(topic);
    if (!set) return;
    set.delete(listener);
    if (set.size === 0) {
      this.listeners.delete(topic);
      this.sendUnsubscribe([topic]);
    }
  }

  /** Publish a value on `topic` to the server. */
  publish(topic: string, value: unknown): void {
    if (!this.wasmReady || !this.socket || this.socket.readyState !== WebSocket.OPEN) {
      return;
    }
    try {
      const bytes = encodePayload(this.codecKind, topic, value);
      this.socket.send(bytes);
    } catch (err) {
      console.warn("wingfoil: publish failed on", topic, err);
    }
  }

  /** Observe the underlying connection state. */
  onConnection(listener: ConnectionListener): () => void {
    this.connListeners.add(listener);
    return () => this.connListeners.delete(listener);
  }

  /** Server-reported wire protocol version, once Hello was received. */
  get version(): number | null {
    return this.serverVersion;
  }

  // ---- internals ----

  private async boot() {
    if (!this.wasmReady) {
      if (this.opts.wasmUrl) {
        await init(this.opts.wasmUrl);
      } else {
        await init();
      }
      init_panic_hook();
      this.wasmReady = true;
    }
    this.connect();
  }

  private connect() {
    if (this.closed) return;
    this.emitConn({ kind: "connecting" });
    const socket = new WebSocket(this.opts.url);
    socket.binaryType = "arraybuffer";
    this.socket = socket;

    socket.addEventListener("message", (ev) => this.onMessage(ev.data));
    socket.addEventListener("error", (ev) => this.emitConn({ kind: "error", error: ev }));
    socket.addEventListener("close", (ev) => {
      this.emitConn({ kind: "closed", code: ev.code, reason: ev.reason });
      if (!this.closed && this.opts.reconnectMs > 0) {
        setTimeout(() => this.connect(), this.opts.reconnectMs);
      }
    });
    socket.addEventListener("open", () => {
      // Re-send any existing subscriptions on reconnect.
      const topics = Array.from(this.listeners.keys());
      if (topics.length > 0) this.sendSubscribe(topics);
    });
  }

  private onMessage(data: ArrayBuffer | Blob | string) {
    if (typeof data === "string") {
      data = new TextEncoder().encode(data).buffer as ArrayBuffer;
    } else if (data instanceof Blob) {
      // Unexpected: we set binaryType='arraybuffer'. Ignore.
      return;
    }
    const bytes = new Uint8Array(data as ArrayBuffer);
    let env: Envelope;
    try {
      env = decodeEnvelope(this.codecKind, bytes) as Envelope;
    } catch (err) {
      console.warn("wingfoil: envelope decode failed", err);
      return;
    }
    if (env.topic === controlTopic()) {
      this.handleControl(env.payload);
      return;
    }
    const listeners = this.listeners.get(env.topic);
    if (!listeners || listeners.size === 0) return;
    let payloadValue: unknown;
    try {
      payloadValue = decodePayload(this.codecKind, env.payload);
    } catch (err) {
      console.warn("wingfoil: payload decode failed on", env.topic, err);
      return;
    }
    for (const fn of listeners) {
      try {
        fn(payloadValue, env.timeNs);
      } catch (err) {
        console.warn("wingfoil: listener threw", err);
      }
    }
  }

  private handleControl(payload: Uint8Array) {
    try {
      const ctrl = decodeControl(this.codecKind, payload) as {
        Hello?: { codec: string; version: number };
      };
      if (ctrl.Hello) {
        this.serverVersion = ctrl.Hello.version;
        this.emitConn({
          kind: "open",
          codec: ctrl.Hello.codec as CodecKind,
          version: ctrl.Hello.version,
        });
      }
    } catch (err) {
      console.warn("wingfoil: control decode failed", err);
    }
  }

  private sendSubscribe(topics: string[]) {
    if (!this.wasmReady || !this.socket || this.socket.readyState !== WebSocket.OPEN) return;
    try {
      this.socket.send(encodeSubscribe(this.codecKind, topics));
    } catch (err) {
      console.warn("wingfoil: subscribe encode failed", err);
    }
  }

  private sendUnsubscribe(topics: string[]) {
    if (!this.wasmReady || !this.socket || this.socket.readyState !== WebSocket.OPEN) return;
    try {
      this.socket.send(encodeUnsubscribe(this.codecKind, topics));
    } catch (err) {
      console.warn("wingfoil: unsubscribe encode failed", err);
    }
  }

  private emitConn(state: ConnectionState) {
    for (const fn of this.connListeners) {
      try {
        fn(state);
      } catch {
        // ignore
      }
    }
  }
}

export { wireVersion };

// ── Utilities ─────────────────────────────────────────────────────────────
//
// Small browser helpers used by the tracing module and frequently needed
// by consumers building latency-aware UIs against a wingfoil server.

/**
 * Generate a fresh 16-byte UUID v4 as a `Uint8Array`. Suitable for use as
 * the `session: [u8; 16]` field on outbound frames; sent verbatim through
 * the JSON codec as an array of integers.
 */
export function newSessionId(): Uint8Array {
  const u = (typeof crypto.randomUUID === "function"
    ? crypto.randomUUID()
    : fallbackUuid()
  ).replaceAll("-", "");
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i++) out[i] = parseInt(u.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** Format a 16-byte session ID (or any byte array) as a lowercase hex string. */
export function sessionHex(bytes: Uint8Array | ArrayLike<number>): string {
  const arr = bytes instanceof Uint8Array ? bytes : Array.from(bytes);
  return Array.from(arr, (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * High-resolution wall-clock timestamp in nanoseconds, derived from
 * `performance.timeOrigin + performance.now()`. Precision is bounded by
 * the browser's `performance.now()` resolution (typically µs); the value
 * is truncated to a safe integer for JS Number arithmetic.
 */
export function nowNs(): number {
  return Math.round(performance.timeOrigin * 1e6 + performance.now() * 1e6);
}

function fallbackUuid(): string {
  const r = new Uint8Array(16);
  crypto.getRandomValues(r);
  r[6] = (r[6] & 0x0f) | 0x40;
  r[8] = (r[8] & 0x3f) | 0x80;
  const h = Array.from(r, (b) => b.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}
