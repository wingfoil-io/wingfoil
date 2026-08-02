// Small browser helpers for @wingfoil/client. Kept in their own module so
// the tracing helpers — and tests — don't drag in the wasm decoder.

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

// `crypto.randomUUID` shipped in Node 19 / Safari 15.4 / Chrome 92, so this
// branch is rarely hit in practice. Both `randomUUID` and `getRandomValues`
// require a secure context; if neither is available, callers will see a
// `TypeError` at first use rather than silently colliding session IDs.
function fallbackUuid(): string {
  const r = new Uint8Array(16);
  crypto.getRandomValues(r);
  r[6] = (r[6] & 0x0f) | 0x40;
  r[8] = (r[8] & 0x3f) | 0x80;
  const h = Array.from(r, (b) => b.toString(16).padStart(2, "0")).join("");
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-${h.slice(12, 16)}-${h.slice(16, 20)}-${h.slice(20)}`;
}
