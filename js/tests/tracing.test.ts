// Unit tests for `LatencyTracker`. We don't import `WingfoilClient` itself
// — `tracing.ts` only depends on its `subscribe` / `publish` / `onConnection`
// surface — so a tiny fake stands in. This keeps the wasm decoder out of
// the test process.

import { describe, expect, it } from "vitest";
import {
  LatencyTracker,
  type RoundTrip,
  type TracingFields,
} from "../src/tracing.js";
import { nowNs } from "../src/utils.js";
import type { WingfoilClient } from "../src/index.js";

interface PublishedFrame {
  topic: string;
  value: unknown;
}

class FakeClient {
  readonly published: PublishedFrame[] = [];
  private readonly listeners = new Map<string, Set<(v: unknown) => void>>();

  subscribe(topic: string, listener: (v: unknown) => void): () => void {
    const set = this.listeners.get(topic) ?? new Set();
    set.add(listener);
    this.listeners.set(topic, set);
    return () => {
      set.delete(listener);
      if (set.size === 0) this.listeners.delete(topic);
    };
  }

  publish(topic: string, value: unknown): void {
    this.published.push({ topic, value });
  }

  /** Push a fake inbound frame, as if it had arrived over the WebSocket. */
  deliver(topic: string, value: unknown): void {
    const set = this.listeners.get(topic);
    if (!set) return;
    for (const fn of set) fn(value);
  }

  hasListener(topic: string): boolean {
    return (this.listeners.get(topic)?.size ?? 0) > 0;
  }
}

function makeTracker(
  client: FakeClient,
  opts: Partial<{ session: Uint8Array; echo: string; fields: Partial<TracingFields> }> = {},
) {
  return new LatencyTracker({
    client: client as unknown as WingfoilClient,
    outbound: "orders",
    inbound: "fills",
    echo: opts.echo,
    session: opts.session,
    fields: opts.fields,
  });
}

const SESSION = new Uint8Array(16).map((_, i) => i + 1);
const sessionArr = Array.from(SESSION);

describe("LatencyTracker.constructor", () => {
  it("defaults to a fresh 16-byte session ID", () => {
    const t = makeTracker(new FakeClient());
    expect(t.session).toBeInstanceOf(Uint8Array);
    expect(t.session.length).toBe(16);
    expect(t.sessionHex).toMatch(/^[0-9a-f]{32}$/);
  });

  it("accepts a caller-provided session", () => {
    const t = makeTracker(new FakeClient(), { session: SESSION });
    expect(Array.from(t.session)).toEqual(sessionArr);
    expect(t.sessionHex).toBe("0102030405060708090a0b0c0d0e0f10");
  });

  it("rejects a session of the wrong byte length", () => {
    expect(() => makeTracker(new FakeClient(), { session: new Uint8Array(8) })).toThrow(
      /16 bytes/,
    );
  });
});

describe("LatencyTracker.send", () => {
  it("stamps session, client_seq, and t_client_send onto every publish", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION });

    expect(t.send({ side: 0, qty: 1 })).toBe(1);
    expect(t.send({ side: 1, qty: 2 })).toBe(2);

    expect(c.published).toHaveLength(2);
    const [a, b] = c.published as { topic: string; value: Record<string, unknown> }[];

    expect(a.topic).toBe("orders");
    expect(a.value.session).toEqual(sessionArr);
    expect(a.value.client_seq).toBe(1);
    expect(a.value.side).toBe(0);
    expect(a.value.qty).toBe(1);
    expect(typeof a.value.t_client_send).toBe("number");

    expect(b.value.client_seq).toBe(2);
  });

  it("auto-stamped fields override caller fields of the same name", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION });
    t.send({ session: [0, 0, 0], client_seq: 999, t_client_send: 0, qty: 7 });
    const v = (c.published[0].value as Record<string, unknown>);
    expect(v.session).toEqual(sessionArr);
    expect(v.client_seq).toBe(1);
    expect(v.t_client_send).not.toBe(0);
    expect(v.qty).toBe(7);
  });

  it("respects field-name overrides on publish", () => {
    const c = new FakeClient();
    const t = makeTracker(c, {
      session: SESSION,
      fields: { clientSeq: "cseq", tClientSend: "tsend" },
    });
    t.send({ side: 0 });
    const v = c.published[0].value as Record<string, unknown>;
    expect(v.cseq).toBe(1);
    expect(typeof v.tsend).toBe("number");
    expect(v.client_seq).toBeUndefined();
  });
});

describe("LatencyTracker.onResponse", () => {
  it("filters out responses for other sessions", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION });
    const seen: RoundTrip[] = [];
    t.onResponse((rt) => seen.push(rt));

    c.deliver("fills", {
      session: Array.from(new Uint8Array(16).fill(0xff)),
      client_seq: 1,
      t_client_send: 0,
      stamps: [10, 20],
    });
    expect(seen).toHaveLength(0);
  });

  it("computes RTT, server-resident, and wire deltas", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION });
    const seen: RoundTrip[] = [];
    t.onResponse((rt) => seen.push(rt));

    // Pin t_client_send to a value definitely earlier than nowNs() will report.
    c.deliver("fills", {
      session: sessionArr,
      client_seq: 42,
      t_client_send: 1,
      stamps: [100, 250],
    });

    expect(seen).toHaveLength(1);
    const rt = seen[0];
    expect(rt.clientSeq).toBe(42);
    expect(rt.tClientSend).toBe(1);
    expect(rt.tClientRecv).toBeGreaterThan(rt.tClientSend);
    expect(rt.rttNs).toBe(rt.tClientRecv - rt.tClientSend);
    expect(rt.serverResidentNs).toBe(150); // 250 - 100
    expect(rt.wireRttNs).toBe(Math.max(0, rt.rttNs - 150));
    expect(rt.stamps).toEqual([100, 250]);
  });

  it("yields serverResidentNs=0 when stamps has fewer than two entries", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION });
    const seen: RoundTrip[] = [];
    t.onResponse((rt) => seen.push(rt));

    c.deliver("fills", { session: sessionArr, client_seq: 1, t_client_send: 0, stamps: [] });
    c.deliver("fills", { session: sessionArr, client_seq: 2, t_client_send: 0, stamps: [42] });

    expect(seen[0].serverResidentNs).toBe(0);
    expect(seen[1].serverResidentNs).toBe(0);
  });

  it("clamps negative RTT/wire deltas at zero", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION });
    const seen: RoundTrip[] = [];
    t.onResponse((rt) => seen.push(rt));

    // t_client_send 10s in the future ⇒ raw RTT would be negative.
    const future = nowNs() + 10_000_000_000;
    c.deliver("fills", {
      session: sessionArr,
      client_seq: 1,
      t_client_send: future,
      stamps: [10, 20],
    });
    expect(seen[0].rttNs).toBe(0);
    expect(seen[0].wireRttNs).toBe(0);
  });

  it("echoes back BEFORE invoking the listener (so a throwing listener can't starve the echo)", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION, echo: "latency_echo" });
    const order: string[] = [];
    t.onResponse(() => {
      order.push("listener");
      throw new Error("boom");
    });

    expect(() =>
      c.deliver("fills", {
        session: sessionArr,
        client_seq: 7,
        t_client_send: 1,
        stamps: [10, 20],
      }),
    ).toThrow(/boom/);

    expect(c.published).toHaveLength(1);
    const echo = c.published[0];
    expect(echo.topic).toBe("latency_echo");
    const v = echo.value as Record<string, unknown>;
    expect(v.session).toEqual(sessionArr);
    expect(v.client_seq).toBe(7);
    expect(v.t_client_send).toBe(1);
    expect(v.stamps).toEqual([10, 20]);
    expect(typeof v.t_client_recv).toBe("number");
    expect(order).toEqual(["listener"]);
  });

  it("does not echo when no echo topic is configured", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION });
    t.onResponse(() => {});
    c.deliver("fills", { session: sessionArr, client_seq: 1, t_client_send: 0, stamps: [1, 2] });
    expect(c.published).toHaveLength(0);
  });

  it("returned unsubscribe removes the listener and untracks it", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION });
    let calls = 0;
    const unsub = t.onResponse(() => {
      calls += 1;
    });
    c.deliver("fills", { session: sessionArr, client_seq: 1, t_client_send: 0, stamps: [] });
    unsub();
    c.deliver("fills", { session: sessionArr, client_seq: 2, t_client_send: 0, stamps: [] });
    expect(calls).toBe(1);
    expect(c.hasListener("fills")).toBe(false);
  });
});

describe("LatencyTracker.close", () => {
  it("tears down all listeners and gates further send/onResponse calls", () => {
    const c = new FakeClient();
    const t = makeTracker(c, { session: SESSION, echo: "latency_echo" });
    let calls = 0;
    t.onResponse(() => {
      calls += 1;
    });

    t.close();

    expect(c.hasListener("fills")).toBe(false);
    expect(t.send({ side: 0 })).toBe(1); // returns the seq, but doesn't publish
    expect(c.published).toHaveLength(0);
    // re-subscribing post-close is a no-op
    const noopUnsub = t.onResponse(() => {
      calls += 1;
    });
    expect(c.hasListener("fills")).toBe(false);
    noopUnsub(); // still callable
  });

  it("is idempotent", () => {
    const t = makeTracker(new FakeClient(), { session: SESSION });
    t.close();
    expect(() => t.close()).not.toThrow();
  });
});
