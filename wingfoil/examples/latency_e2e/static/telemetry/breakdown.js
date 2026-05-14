// Rolling-mean breakdown tiles.
//
// Each bucket: { el, value: (rt) => number, format?: (ns) => string }
// The mean is over the last `window` samples so the headline numbers
// don't jitter on every fill.

import { fmtUs } from './format.js';

export class BreakdownPanel {
  constructor({ buckets, window: win = 32 }) {
    if (!Array.isArray(buckets) || buckets.length === 0) {
      throw new Error('BreakdownPanel: buckets must be a non-empty array');
    }
    this.window = win;
    this._buckets = buckets.map((b) => ({
      el: b.el,
      value: b.value,
      format: b.format ?? fmtUs,
      buf: [],
    }));
  }

  push(rt) {
    for (const b of this._buckets) {
      b.buf.push(b.value(rt));
      if (b.buf.length > this.window) b.buf.shift();
      let s = 0; for (const v of b.buf) s += v;
      b.el.textContent = b.format(s / b.buf.length);
    }
  }

  destroy() {}
}
