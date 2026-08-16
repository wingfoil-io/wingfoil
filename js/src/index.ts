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
} from "./wasm/wingfoil_wasm.js";

export type CodecKind = "bincode" | "json";

/** Envelope shape as surfaced by the wasm decoder. */
export interface Envelope {
  topic: string;
  timeNs: bigint;
  payload: Uint8Array;
}

/**
 * Callback invoked with the latest value of each frame's burst. A frame
 * carries a *burst* — the values that share one `timeNs` — and the
 * default `subscribe` collapses it to the last (latest) value, which is
 * the right default for "show the current value". Use
 * {@link WingfoilClient.subscribeBurst} to receive the whole group.
 */
export type TopicListener = (value: unknown, timeNs: bigint) => void;

/**
 * Callback invoked with the whole burst — every value that shares one
 * `timeNs`. A scalar payload is a one-element burst; a graph that publishes
 * a `Stream<Vec<T>>` sends several values at one timestamp as one frame.
 */
export type BurstListener = (values: unknown[], timeNs: bigint) => void;

/**
 * Normalize a decoded payload into a burst array. A payload that decodes
 * to an array is the same-`timeNs` group as-is; a scalar payload (number,
 * struct, …) is wrapped as a single-element burst so scalar and burst
 * subscribers behave uniformly. Exposed for testing.
 */
export function normalizeBurst(decoded: unknown): unknown[] {
  return Array.isArray(decoded) ? (decoded as unknown[]) : [decoded];
}

/**
 * Callback invoked when the server signals a topic's stream has ended
 * (a [`ControlMessage::Complete`], e.g. a historical replay finishing).
 * After this fires no further values will arrive on the topic.
 */
export type CompleteListener = (topic: string) => void;

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
   * Wire codec the server is using. **Defaults to `"json"`**, and `"json"`
   * is the only value this client accepts — `"bincode"` is rejected by the
   * {@link WingfoilClient} constructor, see {@link resolveCodec}.
   *
   * **Data payloads require `"json"`.** A browser has no Rust schema, and
   * bincode is schema-driven in both directions: a JS object encodes as a
   * length-prefixed map that the server's `deserialize_struct` reads as
   * silent garbage, and bincode bytes cannot be decoded here at all. So
   * {@link WingfoilClient.publish} and payload decoding cannot work under
   * `"bincode"`. Envelope and `$ctrl` control frames — fixed shapes both
   * sides know — work under either codec, which is exactly why a bincode
   * connection *looks* healthy while every data frame fails.
   *
   * The default used to be `"bincode"`, to match the server's default. That
   * is the wrong trade for a browser: the envelope codec has to match the
   * server either way, so a browser user configures **both** sides whatever
   * the default is — and of the two matching pairs, only JSON/JSON carries a
   * payload. A default the browser can never use is not a useful default, so
   * the one that works is the one we pick. Start the server with
   * `.codec(CodecKind::Json)` whenever a browser is a peer.
   */
  codec?: CodecKind;
  /**
   * Reconnect on close with this delay. Defaults to 1000 ms; `0` disables.
   *
   * A *clean* end is never reconnected regardless of this value: once the
   * server signals end-of-stream (a `Complete` control frame, e.g. a
   * historical replay finishing) or closes the socket with a normal code
   * (1000 / 1001, "going away"), the client treats the session as done and
   * stops. Only an abnormal drop (e.g. 1006) triggers the reconnect timer.
   * This prevents a finished historical stream from reconnect-looping
   * against a server that has intentionally shut down.
   */
  reconnectMs?: number;
  /** Optional WASM module URL override (advanced). */
  wasmUrl?: string | URL;
}

/**
 * Why the browser cannot carry a **data payload** under the bincode codec,
 * and what to do instead.
 *
 * Verbatim copy of `BINCODE_PAYLOAD_UNSUPPORTED` in
 * `crates/wingfoil-wasm/src/lib.rs`, which is what `encodePayload` /
 * `decodePayload` raise per frame. A `pub const` is not part of the
 * `wasm-bindgen` surface, so it cannot be imported; keep the two in step.
 */
export const BINCODE_PAYLOAD_UNSUPPORTED =
  "the bincode codec cannot carry browser data payloads. bincode is " +
  "schema-driven and non-self-describing, so a JS value encodes as a " +
  "length-prefixed map while the server's `deserialize_struct` expects bare " +
  "fields in declaration order — the server would decode silent garbage rather " +
  "than report an error — and a server-encoded struct cannot be decoded in the " +
  "browser at all without its Rust schema. Use the JSON codec for browser " +
  "payloads: start the server with `.codec(CodecKind::Json)` and construct the " +
  'client with `codec: "json"`. Envelope and control frames are unaffected; ' +
  "this applies only to user data payloads.";

/**
 * Resolve {@link ClientOptions.codec} to the codec the client will use,
 * rejecting one it can never carry a payload under. Exposed for testing; the
 * {@link WingfoilClient} constructor applies it.
 *
 * The diagnosis belongs at the line that *chose* the codec, not at every
 * frame that then fails. Under `"bincode"` the socket connects, subscribes
 * and receives envelopes — those shapes are fixed and known to both sides —
 * while each publish and each inbound data frame is dropped with a
 * `console.warn`, forever. Throwing here turns that endless stream of
 * warnings into one error naming both halves of the fix.
 *
 * With the default now `"json"` this only fires on an explicit
 * `codec: "bincode"`, and it fires anyway: an explicit choice the browser
 * cannot honour must fail loudly rather than silently.
 */
export function resolveCodec(codec: CodecKind | undefined): CodecKind {
  const resolved = codec ?? "json";
  if (resolved === "bincode") {
    throw new Error(`WingfoilClient: ${BINCODE_PAYLOAD_UNSUPPORTED}`);
  }
  return resolved;
}

/** WebSocket close codes that indicate a deliberate, clean shutdown. */
const CLEAN_CLOSE_CODES = new Set<number>([1000, 1001]);

/**
 * Narrow a wasm-returned byte array to one backed by a plain `ArrayBuffer`.
 *
 * `wasm-bindgen` declares its byte returns as a bare `Uint8Array`, which since
 * TypeScript 5.7 widens to `Uint8Array<ArrayBufferLike>` — a union that admits
 * `SharedArrayBuffer` and so is not assignable to `WebSocket.send`'s
 * `BufferSource`. TypeScript 7 enforces that where 5.9 let it slide. The
 * narrowing is sound: `wasm-bindgen` copies each of these out of wasm linear
 * memory into a freshly allocated, non-shared `ArrayBuffer`.
 */
function asBufferSource(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  return bytes as Uint8Array<ArrayBuffer>;
}

/**
 * Decide whether a closed socket should trigger a reconnect. Exposed for
 * testing; the client applies it in its `close` handler.
 *
 * Reconnect only on an abnormal drop that the caller still wants retried.
 * A clean finish — the user closed the client, the stream completed, the
 * server closed with a normal code, or reconnect is disabled — never
 * reconnects, so a finished historical replay does not loop against a
 * server that has intentionally shut down.
 */
export function shouldReconnect(params: {
  userClosed: boolean;
  streamFinished: boolean;
  closeCode: number;
  reconnectMs: number;
}): boolean {
  const { userClosed, streamFinished, closeCode, reconnectMs } = params;
  if (userClosed || streamFinished || reconnectMs <= 0) return false;
  return !CLEAN_CLOSE_CODES.has(closeCode);
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
  private readonly burstListeners = new Map<string, Set<BurstListener>>();
  private readonly completeListeners = new Set<CompleteListener>();
  private readonly connListeners = new Set<ConnectionListener>();
  private codecKind: CodecKind;
  private serverVersion: number | null = null;
  /**
   * Set once the server signals end-of-stream for any topic. A finished
   * stream must not reconnect — the server is done, not merely dropped.
   */
  private streamFinished = false;

  /**
   * Construct a client and start connecting.
   *
   * @throws if {@link ClientOptions.codec} is `"bincode"` — a codec no
   * browser data payload can travel under, in either direction. The default
   * is `"json"`, so this only fires on an explicit choice.
   */
  constructor(options: ClientOptions) {
    this.opts = {
      url: options.url,
      // Throws on `"bincode"`, before a socket is opened — see `resolveCodec`.
      codec: resolveCodec(options.codec),
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

  /**
   * Subscribe to `topic`, receiving the latest value of each frame's
   * burst. Returns an unsubscribe function. Use {@link subscribeBurst} to
   * receive every value in a same-`timeNs` group.
   */
  subscribe(topic: string, listener: TopicListener): () => void {
    const fresh = !this.hasTopicListener(topic);
    const set = this.listeners.get(topic) ?? new Set<TopicListener>();
    set.add(listener);
    this.listeners.set(topic, set);
    if (fresh) this.sendSubscribe([topic]);
    return () => this.unsubscribe(topic, listener);
  }

  /** Remove a scalar listener; send an unsubscribe frame if the topic is now idle. */
  unsubscribe(topic: string, listener: TopicListener): void {
    const set = this.listeners.get(topic);
    if (!set) return;
    set.delete(listener);
    if (set.size === 0) this.listeners.delete(topic);
    if (!this.hasTopicListener(topic)) this.sendUnsubscribe([topic]);
  }

  /**
   * Subscribe to `topic`, receiving the whole burst (every value that
   * shares a `timeNs`) per frame. Returns an unsubscribe function. This is
   * the full-fidelity form — no values are collapsed — useful for charting
   * a historical replay where several points may land on one timestamp.
   */
  subscribeBurst(topic: string, listener: BurstListener): () => void {
    const fresh = !this.hasTopicListener(topic);
    const set = this.burstListeners.get(topic) ?? new Set<BurstListener>();
    set.add(listener);
    this.burstListeners.set(topic, set);
    if (fresh) this.sendSubscribe([topic]);
    return () => this.unsubscribeBurst(topic, listener);
  }

  /** Remove a burst listener; send an unsubscribe frame if the topic is now idle. */
  unsubscribeBurst(topic: string, listener: BurstListener): void {
    const set = this.burstListeners.get(topic);
    if (!set) return;
    set.delete(listener);
    if (set.size === 0) this.burstListeners.delete(topic);
    if (!this.hasTopicListener(topic)) this.sendUnsubscribe([topic]);
  }

  /** True when `topic` has any scalar or burst listener attached. */
  private hasTopicListener(topic: string): boolean {
    return this.listeners.has(topic) || this.burstListeners.has(topic);
  }

  /** Every topic with at least one listener (used to resubscribe on reconnect). */
  private subscribedTopics(): string[] {
    return Array.from(
      new Set([...this.listeners.keys(), ...this.burstListeners.keys()]),
    );
  }

  /**
   * Publish a value on `topic` to the server.
   *
   * **Requires the JSON codec** ({@link ClientOptions.codec} `= "json"` —
   * the default — and the server started with `.codec(CodecKind::Json)`).
   * A client can no longer reach this method under `"bincode"`: the
   * constructor rejects that codec outright, because the browser cannot
   * produce a bincode encoding of a Rust struct and the server would decode
   * silent garbage if it tried. So the only publishes that fail here are
   * values JSON itself cannot carry.
   *
   * Failures never propagate: an unencodable value, or a socket that is not
   * open, is warned about and dropped.
   */
  publish(topic: string, value: unknown): void {
    if (!this.wasmReady || !this.socket || this.socket.readyState !== WebSocket.OPEN) {
      return;
    }
    try {
      const bytes = encodePayload(this.codecKind, topic, value);
      this.socket.send(asBufferSource(bytes));
    } catch (err) {
      console.warn("wingfoil: publish failed on", topic, err);
    }
  }

  /** Observe the underlying connection state. */
  onConnection(listener: ConnectionListener): () => void {
    this.connListeners.add(listener);
    return () => this.connListeners.delete(listener);
  }

  /**
   * Observe end-of-stream signals. The listener fires with the topic name
   * each time the server sends a `Complete` control frame — for example
   * when a historical replay reaches the end of its source. Returns a
   * function that removes the listener.
   */
  onComplete(listener: CompleteListener): () => void {
    this.completeListeners.add(listener);
    return () => this.completeListeners.delete(listener);
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
      // Do not reconnect after a clean finish: the stream completed, the
      // user closed us, reconnect is disabled, or the server closed the
      // socket deliberately (a normal close code). Only an abnormal drop
      // reconnects. This stops a finished historical replay from looping
      // against a server that has intentionally shut down.
      if (
        shouldReconnect({
          userClosed: this.closed,
          streamFinished: this.streamFinished,
          closeCode: ev.code,
          reconnectMs: this.opts.reconnectMs,
        })
      ) {
        setTimeout(() => this.connect(), this.opts.reconnectMs);
      }
    });
    socket.addEventListener("open", () => {
      // Re-send any existing subscriptions on reconnect.
      const topics = this.subscribedTopics();
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
    const scalar = this.listeners.get(env.topic);
    const bursts = this.burstListeners.get(env.topic);
    if (!scalar?.size && !bursts?.size) return;

    let decoded: unknown;
    try {
      decoded = decodePayload(this.codecKind, env.payload);
    } catch (err) {
      console.warn("wingfoil: payload decode failed on", env.topic, err);
      return;
    }
    // Each frame's payload is a burst (array) of same-`timeNs` values.
    const values = normalizeBurst(decoded);
    if (values.length === 0) return;

    if (bursts?.size) {
      for (const fn of bursts) {
        try {
          fn(values, env.timeNs);
        } catch (err) {
          console.warn("wingfoil: burst listener threw", err);
        }
      }
    }
    if (scalar?.size) {
      // Collapse the burst to its latest value for scalar subscribers.
      const latest = values[values.length - 1];
      for (const fn of scalar) {
        try {
          fn(latest, env.timeNs);
        } catch (err) {
          console.warn("wingfoil: listener threw", err);
        }
      }
    }
  }

  private handleControl(payload: Uint8Array) {
    try {
      const ctrl = decodeControl(this.codecKind, payload) as {
        Hello?: { codec: string; version: number };
        Complete?: { topic: string };
      };
      if (ctrl.Hello) {
        this.serverVersion = ctrl.Hello.version;
        this.emitConn({
          kind: "open",
          codec: ctrl.Hello.codec as CodecKind,
          version: ctrl.Hello.version,
        });
      } else if (ctrl.Complete) {
        this.handleComplete(ctrl.Complete.topic);
      }
    } catch (err) {
      console.warn("wingfoil: control decode failed", err);
    }
  }

  private handleComplete(topic: string) {
    // A completed stream is finished for good — don't reconnect when the
    // server subsequently closes the socket.
    this.streamFinished = true;
    for (const fn of this.completeListeners) {
      try {
        fn(topic);
      } catch (err) {
        console.warn("wingfoil: complete listener threw", err);
      }
    }
  }

  private sendSubscribe(topics: string[]) {
    if (!this.wasmReady || !this.socket || this.socket.readyState !== WebSocket.OPEN) return;
    try {
      this.socket.send(asBufferSource(encodeSubscribe(this.codecKind, topics)));
    } catch (err) {
      console.warn("wingfoil: subscribe encode failed", err);
    }
  }

  private sendUnsubscribe(topics: string[]) {
    if (!this.wasmReady || !this.socket || this.socket.readyState !== WebSocket.OPEN) return;
    try {
      this.socket.send(asBufferSource(encodeUnsubscribe(this.codecKind, topics)));
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

// Re-export the small browser helpers from `./utils.js` so existing
// consumers don't have to know about the split. The helpers live in
// their own file so the tracing module — and tests — don't drag in the
// wasm decoder.
export { newSessionId, sessionHex, nowNs } from "./utils.js";
