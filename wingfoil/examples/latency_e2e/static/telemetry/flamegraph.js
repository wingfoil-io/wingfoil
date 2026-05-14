// Nested-call-stack flamegraph rendered to <canvas>.
//
// Layers are ordered outermost-first. Layer 0 is the root (widest box);
// each subsequent layer is drawn as a strict sub-interval of the layer
// below it — same shape as a CPU flamegraph, but the samples are
// wall-clock fills. Layer durations are rolling means over the last
// `window` samples so the picture is stable across fills.
//
// Each layer:  { name, detail, color, value: (rt) => number }
//
// Optional elements (any can be omitted):
//   - tipEl:       absolutely-positioned hover tooltip
//   - metaEl:      e.g. "n=32" sample-count badge
//   - legendHost:  per-layer legend rows

import { fmtNs } from './format.js';

const CSS_HEIGHT = 200;

export class FlameGraph {
  constructor({ host, layers, tipEl, metaEl, legendHost, window: win = 32 }) {
    if (!host) throw new Error('FlameGraph: host <canvas> required');
    if (!Array.isArray(layers) || layers.length === 0) {
      throw new Error('FlameGraph: layers must be a non-empty array');
    }
    this.host = host;
    this.layers = layers;
    this.tipEl = tipEl ?? null;
    this.metaEl = metaEl ?? null;
    this.legendHost = legendHost ?? null;
    this.window = win;
    this._buffers = layers.map(() => []);
    this._hover = null;

    this._onMove = this._onMove.bind(this);
    this._onLeave = this._onLeave.bind(this);
    host.addEventListener('mousemove', this._onMove);
    host.addEventListener('mouseleave', this._onLeave);
    this._ro = new ResizeObserver(() => this._draw());
    this._ro.observe(host);

    this._renderLegend();
    this._draw();
  }

  push(rt) {
    for (let i = 0; i < this.layers.length; i++) {
      const buf = this._buffers[i];
      buf.push(this.layers[i].value(rt));
      if (buf.length > this.window) buf.shift();
    }
    if (this.metaEl) this.metaEl.textContent = `n=${this._buffers[0].length}`;
    this._draw();
  }

  destroy() {
    this._ro?.disconnect();
    this.host.removeEventListener('mousemove', this._onMove);
    this.host.removeEventListener('mouseleave', this._onLeave);
  }

  // ── internals ──────────────────────────────────────────────────────────

  _mean(buf) {
    if (buf.length === 0) return 0;
    let s = 0; for (const v of buf) s += v;
    return s / buf.length;
  }

  // Layout: each layer is centered within its parent box, so the padding
  // either side equals (parent − child) / 2. For layer 1 onward the
  // child offset isn't measured (potentially different clocks) — same
  // convention every flamegraph viewer uses when child offsets are unknown.
  _boxes() {
    const totals = this.layers.map((_, i) => this._mean(this._buffers[i]));
    const root = totals[0] || 1;
    const boxes = [];
    let prevStart = 0, prevWidth = root;
    for (let i = 0; i < this.layers.length; i++) {
      const dur = Math.min(totals[i], prevWidth);
      const offset = (prevWidth - dur) / 2;
      const start = prevStart + offset;
      boxes.push({ layer: this.layers[i], ns: totals[i], start, width: dur });
      prevStart = start;
      prevWidth = dur;
    }
    return { boxes, root };
  }

  _canvas() {
    const c = this.host;
    const dpr = window.devicePixelRatio || 1;
    const cssW = c.clientWidth;
    // Don't read `c.height` back: setting it reflects into the attribute,
    // so on the second render cssH would be the previous bitmap height
    // (e.g. 400 at dpr=2), then 800, then 1600 — the canvas grows every
    // frame and the panel flickers. Keep CSS height as a constant.
    const cssH = CSS_HEIGHT;
    if (c.width !== cssW * dpr || c.height !== cssH * dpr) {
      c.width = cssW * dpr; c.height = cssH * dpr;
      c.style.height = cssH + 'px';
    }
    const ctx = c.getContext('2d');
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { ctx, w: cssW, h: cssH };
  }

  _draw() {
    const { ctx, w, h } = this._canvas();
    ctx.clearRect(0, 0, w, h);

    if (this._buffers[0].length === 0) {
      ctx.fillStyle = '#8b949e';
      ctx.font = '12px system-ui, sans-serif';
      ctx.textBaseline = 'top';
      ctx.fillText('waiting for samples…', 10, 10);
      return;
    }

    const { boxes, root } = this._boxes();
    const pad = 4;
    const rowH = Math.floor((h - pad * 2) / this.layers.length);
    const px = (ns) => (ns / root) * (w - pad * 2);

    // Canvas y grows downward; layer 0 sits at the bottom row.
    for (let i = 0; i < boxes.length; i++) {
      const b = boxes[i];
      const x = pad + px(b.start);
      const y = h - pad - (i + 1) * rowH;
      const bw = Math.max(1, px(b.width));
      const bh = rowH - 1;

      ctx.fillStyle = b.layer.color;
      ctx.globalAlpha = (this._hover === null || this._hover === i) ? 1.0 : 0.55;
      ctx.fillRect(x, y, bw, bh);
      ctx.globalAlpha = 1.0;

      const label = `${b.layer.name} · ${fmtNs(b.ns)}`;
      ctx.font = '11px ui-monospace, monospace';
      const tw = ctx.measureText(label).width;
      if (tw + 8 < bw) {
        ctx.fillStyle = '#0d1117';
        ctx.textBaseline = 'middle';
        ctx.fillText(label, x + 4, y + bh / 2);
      } else if (bw > 36) {
        const short = b.layer.name;
        const stw = ctx.measureText(short).width;
        if (stw + 6 < bw) {
          ctx.fillStyle = '#0d1117';
          ctx.textBaseline = 'middle';
          ctx.fillText(short, x + 3, y + bh / 2);
        }
      }
    }
  }

  _hitTest(ev) {
    const c = this.host;
    const rect = c.getBoundingClientRect();
    const x = ev.clientX - rect.left;
    const y = ev.clientY - rect.top;
    const h = CSS_HEIGHT;
    const pad = 4;
    const rowH = Math.floor((h - pad * 2) / this.layers.length);
    const { boxes, root } = this._boxes();
    const w = c.clientWidth;
    const px = (ns) => (ns / root) * (w - pad * 2);
    for (let i = boxes.length - 1; i >= 0; i--) {
      const b = boxes[i];
      const bx = pad + px(b.start);
      const by = h - pad - (i + 1) * rowH;
      const bw = Math.max(1, px(b.width));
      const bh = rowH - 1;
      if (x >= bx && x <= bx + bw && y >= by && y <= by + bh) return { idx: i, box: b };
    }
    return null;
  }

  _onMove(ev) {
    const hit = this._hitTest(ev);
    if (!hit) {
      if (this._hover !== null) { this._hover = null; this._draw(); }
      if (this.tipEl) this.tipEl.style.display = 'none';
      return;
    }
    if (this._hover !== hit.idx) { this._hover = hit.idx; this._draw(); }
    if (!this.tipEl) return;
    const { boxes, root } = this._boxes();
    const parentNs = hit.idx === 0 ? root : (boxes[hit.idx - 1].ns || 1);
    const pctParent = (hit.box.ns / parentNs) * 100;
    const pctRoot = (hit.box.ns / (root || 1)) * 100;
    this.tipEl.innerHTML =
      `<strong>${hit.box.layer.name}</strong> — ${fmtNs(hit.box.ns)}<br>` +
      `${hit.box.layer.detail}<br>` +
      `${pctParent.toFixed(1)}% of parent · ${pctRoot.toFixed(1)}% of total`;
    const wrap = this.host.parentElement;
    const wrapRect = wrap.getBoundingClientRect();
    this.tipEl.style.left = (ev.clientX - wrapRect.left + 12) + 'px';
    this.tipEl.style.top = (ev.clientY - wrapRect.top + 12) + 'px';
    this.tipEl.style.display = 'block';
  }

  _onLeave() {
    this._hover = null;
    if (this.tipEl) this.tipEl.style.display = 'none';
    this._draw();
  }

  _renderLegend() {
    if (!this.legendHost) return;
    this.legendHost.innerHTML = this.layers.map((l) =>
      `<div class="row"><span class="sw" style="background:${l.color}"></span>` +
      `<span class="name">${l.name}</span><span>${l.detail ?? ''}</span></div>`
    ).join('');
  }
}
