// Latency-tracing helpers for @wingfoil/client.
//
// Captures the round-trip pattern that wingfoil's `latency_stages!` /
// `Traced<T, L>` server pipeline expects from a browser client:
//
//   * generate a session UUID per page-load
//   * stamp each outbound request with `client_seq` and `t_client_send`
//   * filter inbound responses to this session
//   * stamp `t_client_recv` on receipt and echo the four timestamps back
//     so the server can compute `rtt_total` and `wire_rtt` within a
//     single clock domain
//
// All four deltas surface on the listener as numbers — no NTP-style
// clock-sync required, because every subtraction lives in one domain.
//
// Codec assumption: requires the server's web adapter to use
// `CodecKind::Json`. The bincode codec serialises a JS `number[]` as a
// length-prefixed `Vec<u8>`, which doesn't match a Rust `[u8; 16]` field.
//
// Field-name overrides (`LatencyTrackerOptions.fields`) apply
// symmetrically to outbound publishes *and* inbound parsing.
//
// Numeric assumption: timestamps and stamps are JS Numbers (not BigInt)
// in the safe-integer range. The wasm JSON decoder produces Numbers
// today; if the wire ever moves to BigInt for u64 fields, this module
// needs a coercion layer.

import type { WingfoilClient } from "./index.js";
import { newSessionId, nowNs, sessionHex } from "./utils.js";

export interface TracingFields {
  session: string;
  clientSeq: string;
  tClientSend: string;
  tClientRecv: string;
  stamps: string;
}

const DEFAULT_FIELDS: TracingFields = {
  session: "session",
  clientSeq: "client_seq",
  tClientSend: "t_client_send",
  tClientRecv: "t_client_recv",
  stamps: "stamps",
};

const SESSION_BYTES = 16;

export interface LatencyTrackerOptions {
  /** The wingfoil client carrying the WebSocket connection. */
  client: WingfoilClient;
  /** Topic to publish requests on (e.g. `"orders"`). */
  outbound: string;
  /** Topic carrying server responses to filter (e.g. `"fills"`). */
  inbound: string;
  /**
   * Optional topic to echo the round-trip back on (e.g. `"latency_echo"`).
   * Omit to disable the echo leg.
   */
  echo?: string;
  /**
   * Existing 16-byte session ID to use; defaults to a fresh random UUID.
   * Useful if the host page already minted one for cross-tab correlation.
   * Throws if length is not 16.
   */
  session?: Uint8Array;
  /**
   * Override individual wire field names. The same map is applied to both
   * outbound publishes (which keys are stamped onto the request) and
   * inbound parsing (which keys the tracker reads off the response).
   */
  fields?: Partial<TracingFields>;
}

/**
 * One inbound response, paired with the latency deltas computed from its
 * timestamps. `payload` is the raw decoded message; `stamps` is a copy of
 * `payload[fields.stamps]` for convenience.
 */
export interface RoundTrip<T = unknown> {
  payload: T;
  clientSeq: number;
  tClientSend: number;
  tClientRecv: number;
  /** Total RTT in ns (client clock: `t_client_recv − t_client_send`). */
  rttNs: number;
  /** Server-side stamps array (length set by the Rust `latency_stages!`). */
  stamps: number[];
  /**
   * `stamps[last] − stamps[0]` (server clock). Zero if `stamps` has fewer
   * than two entries.
   */
  serverResidentNs: number;
  /** `rttNs − serverResidentNs`, clamped at zero (wire RTT, ns). */
  wireRttNs: number;
}

export type RoundTripListener<T> = (rt: RoundTrip<T>) => void;

/**
 * Owns one browser session's outbound counter and round-trip echo loop.
 *
 * @example
 * ```ts
 * const tracker = new LatencyTracker({
 *   client,
 *   outbound: "orders",
 *   inbound:  "fills",
 *   echo:     "latency_echo",
 * });
 * tracker.onResponse<FillFrame>((rt) => {
 *   console.log(rt.rttNs, rt.serverResidentNs, rt.wireRttNs);
 * });
 * tracker.send({ side: 0, qty: 1 });   // session/seq/t_client_send auto-stamped
 * ```
 */
export class LatencyTracker {
  /** Raw 16-byte session UUID used to tag outbound and filter inbound. */
  readonly session: Uint8Array;
  /** Hex form of `session` (32 chars), useful for log / metric labels. */
  readonly sessionHex: string;

  private readonly client: WingfoilClient;
  private readonly outbound: string;
  private readonly inbound: string;
  private readonly echo?: string;
  private readonly fields: TracingFields;
  private readonly sessionArr: number[];
  private seq = 0;
  private closed = false;
  private readonly unsubscribers = new Set<() => void>();

  constructor(opts: LatencyTrackerOptions) {
    if (opts.session && opts.session.length !== SESSION_BYTES) {
      throw new Error(
        `LatencyTracker: session must be ${SESSION_BYTES} bytes, got ${opts.session.length}`,
      );
    }
    this.client = opts.client;
    this.outbound = opts.outbound;
    this.inbound = opts.inbound;
    this.echo = opts.echo;
    this.fields = { ...DEFAULT_FIELDS, ...opts.fields };
    this.session = opts.session ?? newSessionId();
    this.sessionHex = sessionHex(this.session);
    this.sessionArr = Array.from(this.session);
  }

  /**
   * Publish a request on the outbound topic. `session`, `client_seq` and
   * `t_client_send` are stamped automatically and override any same-named
   * keys in `payload`. Returns the `client_seq` that was used.
   *
   * No-op after `close()`; returns the seq that *would* have been used.
   */
  send(payload: Record<string, unknown> = {}): number {
    this.seq += 1;
    if (this.closed) return this.seq;
    const f = this.fields;
    this.client.publish(this.outbound, {
      ...payload,
      [f.session]: this.sessionArr,
      [f.clientSeq]: this.seq,
      [f.tClientSend]: nowNs(),
    });
    return this.seq;
  }

  /**
   * Subscribe to inbound responses for this session only. For each match:
   *
   *   1. `t_client_recv = nowNs()` is stamped
   *   2. the four latency deltas are computed
   *   3. if `echo` was configured, the round-trip is echoed *before* the
   *      listener fires — so a throwing listener can't starve the
   *      latency-report leg
   *   4. the listener is invoked
   *
   * Returns an unsubscribe function. After `close()`, this is a no-op
   * that returns a no-op unsubscribe.
   */
  onResponse<T = unknown>(listener: RoundTripListener<T>): () => void {
    if (this.closed) return () => {};
    const f = this.fields;
    const unsub = this.client.subscribe(this.inbound, (raw) => {
      const msg = raw as Record<string, unknown>;
      const inSession = msg[f.session] as ArrayLike<number> | undefined;
      if (!inSession || !sameSession(inSession, this.sessionArr)) return;
      const tClientRecv = nowNs();
      const tClientSend = numberOr(msg[f.tClientSend], 0);
      const stamps = (msg[f.stamps] as number[] | undefined) ?? [];
      const clientSeq = numberOr(msg[f.clientSeq], 0);
      const rttNs = Math.max(0, tClientRecv - tClientSend);
      const serverResidentNs =
        stamps.length >= 2 ? stamps[stamps.length - 1] - stamps[0] : 0;
      const wireRttNs = Math.max(0, rttNs - serverResidentNs);

      if (this.echo) {
        this.client.publish(this.echo, {
          [f.session]: this.sessionArr,
          [f.clientSeq]: clientSeq,
          [f.tClientSend]: tClientSend,
          [f.tClientRecv]: tClientRecv,
          [f.stamps]: stamps,
        });
      }

      listener({
        payload: msg as T,
        clientSeq,
        tClientSend,
        tClientRecv,
        rttNs,
        stamps,
        serverResidentNs,
        wireRttNs,
      });
    });
    this.unsubscribers.add(unsub);
    return () => {
      unsub();
      this.unsubscribers.delete(unsub);
    };
  }

  /** Tear down all listeners registered through this tracker. Idempotent. */
  close(): void {
    this.closed = true;
    for (const u of this.unsubscribers) u();
    this.unsubscribers.clear();
  }
}

function sameSession(a: ArrayLike<number>, b: ArrayLike<number>): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

function numberOr(v: unknown, fallback: number): number {
  return typeof v === "number" ? v : fallback;
}
