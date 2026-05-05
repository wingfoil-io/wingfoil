// wingfoil latency end-to-end — browser client.
//
// Streams orders, listens for fills, and renders per-hop latency for the
// current browser session. The session UUID, sequence counter, outbound
// timestamps, inbound filtering, RTT/wire deltas, and the echo round-trip
// back to the server are all delegated to `LatencyTracker`.

import { WingfoilClient } from "@wingfoil/client";
import { LatencyTracker } from "@wingfoil/client/tracing";

// Each entry is either a server-clock delta (indices into stamps[]) or
// the derived `wire_rtt`. Every number lives inside a single clock
// domain — no NTP-style sync needed.
const STAGES = [
  ["ws_recv→ws_publish",     "server", 0, 1],
  ["ws_publish→gw_recv",     "server", 1, 2],
  ["gw_recv→gw_price",       "server", 2, 3],
  ["gw_price→fix_send",      "server", 3, 4],
  ["fix_send→fix_recv",      "server", 4, 5],
  ["fix_recv→gw_publish",    "server", 5, 6],
  ["gw_publish→ws_sub_recv", "server", 6, 7],
  ["ws_sub_recv→ws_send",    "server", 7, 8],
  ["wire RTT (out+in)",      "wire"],
];
const COLOURS = [
  "#58a6ff","#3fb950","#f0883e","#f85149",
  "#bc8cff","#79c0ff","#ff7b72","#a371f7",
  "#e3b341",
];
const MAX_POINTS = 200;       // chart history (~100 s at 2 Hz)
const FLAME_WINDOW = 32;      // rolling-mean window for the flamegraph

// Flamegraph layers, bottom (widest, outermost) to top (narrowest, deepest).
// Each layer is a strict sub-interval of the layer below it — that nesting
// is what makes this a flamegraph rather than a waterfall. Server-clock
// indices refer to `stamps[]`; the bottom layer uses the browser clock
// (rttNs) and is centered around the server window.
const FLAME_LAYERS = [
  { name: 'browser',   detail: 'browser → ws_server → browser (full RTT)',     color: '#58a6ff' },
  { name: 'ws_server', detail: 'ws_recv → ws_send (this server, end-to-end)',  color: '#3fb950', from: 0, to: 8 },
  { name: 'iceoryx2',  detail: 'ws_publish → ws_sub_recv (shm + fix_gw)',      color: '#bc8cff', from: 1, to: 7 },
  { name: 'fix_gw',    detail: 'gw_recv → gw_publish (this process)',          color: '#f0883e', from: 2, to: 6 },
  { name: 'lmax',      detail: 'fix_send → fix_recv (FIX/TLS to LMAX + match)', color: '#f85149', from: 4, to: 5 },
];

// ── wingfoil client + latency tracker ─────────────────────────────────────
const client = new WingfoilClient({
  url: `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`,
  codec: 'json',
});

const tracker = new LatencyTracker({
  client,
  outbound: 'orders',
  inbound:  'fills',
  echo:     'latency_echo',
});

document.getElementById('session').textContent = tracker.sessionHex.slice(0, 8) + '…';

// Grafana iframe + pop-out link, both pre-filtered to this session.
(function initGrafana() {
  const origin = `${location.protocol}//${location.hostname}:3000`;
  const url = `${origin}/d/wingfoil-latency-e2e/wingfoil-latency-end-to-end` +
              `?var-session=${tracker.sessionHex}` +
              `&kiosk=tv&theme=dark&refresh=5s&from=now-15m&to=now`;
  document.getElementById('grafana').src = url;
  document.getElementById('grafana-pop').href = url;
})();

// ── UI state ──────────────────────────────────────────────────────────────
let sent = 0, filled = 0, sumPx = 0;
let nextSide = 0;
let chart, chartData, ticker = null;
// Rolling means for the flamegraph: one buffer per layer, plus rttTotal.
// Each buffer holds the last FLAME_WINDOW values; we render the means.
const flameBuffers = FLAME_LAYERS.map(() => []);
const flameRttBuffer = [];
let flameHover = null; // index into FLAME_LAYERS

function setStatus(s) {
  const el = document.getElementById('status');
  el.textContent = s;
  const cls = s === 'live' ? 'live' : s === 'connecting' ? 'connecting' : 'idle';
  el.className = 'pill ' + cls;
}

client.onConnection((s) => {
  if (s.kind === 'open') setStatus('live');
  else if (s.kind === 'connecting') setStatus('connecting');
  else setStatus('disconnected');
});

// ── Per-hop helpers ───────────────────────────────────────────────────────
function stageNs(entry, stamps, rttTotal) {
  const [, kind, a, b] = entry;
  if (kind === 'server') {
    const aVal = stamps[a];
    const bVal = stamps[b];
    if (aVal === null || aVal === undefined || bVal === null || bVal === undefined) return 0;
    return bVal - aVal;
  }
  if (kind === 'wire') {
    if (stamps[0] === null || stamps[0] === undefined || stamps[8] === null || stamps[8] === undefined) return 0;
    return Math.max(0, rttTotal - (stamps[8] - stamps[0]));
  }
  return 0;
}

// ── uPlot chart ───────────────────────────────────────────────────────────
function pushChartPoint(stamps, rttTotal) {
  chartData[0].push(chartData[0].length);
  for (let i = 0; i < STAGES.length; i++) {
    chartData[i + 1].push(stageNs(STAGES[i], stamps, rttTotal));
  }
  while (chartData[0].length > MAX_POINTS) {
    chartData.forEach(s => s.shift());
  }
  // Rewrite x-axis as a contiguous index after shifting.
  chartData[0] = chartData[0].map((_, i) => i);
  chart.setData(chartData);
}

function chartSize() {
  const el = document.getElementById('chart');
  const w = Math.max(280, el.clientWidth);
  const h = Math.max(220, Math.min(420, Math.round(w * 0.42)));
  return { width: w, height: h };
}

function initChart() {
  chartData = [[], ...STAGES.map(() => [])];
  const { width, height } = chartSize();
  const opts = {
    width, height,
    cursor: { drag: { x: true, y: false } },
    scales: { x: { time: false }, y: { distr: 3, log: 10 } },
    legend: { show: true },
    series: [
      { label: 'sample' },
      ...STAGES.map(([name], i) => ({
        label: name,
        stroke: COLOURS[i],
        width: 1.5,
      })),
    ],
    axes: [
      { stroke: '#8b949e' },
      {
        stroke: '#8b949e',
        size: 64,
        values: (_, vs) => vs.map(fmtAxisNs),
      },
    ],
  };
  chart = new uPlot(opts, chartData, document.getElementById('chart'));

  const ro = new ResizeObserver(() => {
    const sz = chartSize();
    chart.setSize(sz);
  });
  ro.observe(document.getElementById('chart'));
}

// ── Flamegraph (nested call stack) ────────────────────────────────────────
// Bottom row = full browser RTT. Each row above is a strict sub-interval
// of the row below it — same shape as a CPU flamegraph, but with wall-clock
// fills as the "samples". Layer durations are rolling means over the last
// FLAME_WINDOW fills so the picture is stable.

function layerNs(layer, stamps) {
  if (layer.from === undefined) return 0;
  const a = stamps[layer.from], b = stamps[layer.to];
  if (a == null || b == null) return 0;
  return Math.max(0, b - a);
}

function pushFlame(stamps, rttNs) {
  for (let i = 0; i < FLAME_LAYERS.length; i++) {
    const ns = i === 0 ? rttNs : layerNs(FLAME_LAYERS[i], stamps);
    const buf = flameBuffers[i];
    buf.push(ns);
    if (buf.length > FLAME_WINDOW) buf.shift();
  }
  flameRttBuffer.push(rttNs);
  if (flameRttBuffer.length > FLAME_WINDOW) flameRttBuffer.shift();
  drawFlamegraph();
  document.getElementById('flamegraph-meta').textContent = `n=${flameRttBuffer.length}`;
}

function mean(buf) {
  if (buf.length === 0) return 0;
  let s = 0; for (const v of buf) s += v;
  return s / buf.length;
}

// Layout: each layer is a horizontal box centered within its parent, so the
// padding either side equals (parent − child) / 2. For L1 (ws_server inside
// browser RTT) the offset isn't measured (different clocks), so we center —
// same convention every flamegraph viewer uses when child offsets are unknown.
function flamegraphBoxes() {
  const totals = FLAME_LAYERS.map((_, i) => mean(flameBuffers[i]));
  const root = totals[0] || 1;
  const boxes = [];
  let prevStart = 0, prevWidth = root;
  for (let i = 0; i < FLAME_LAYERS.length; i++) {
    const dur = Math.min(totals[i], prevWidth);
    const offset = (prevWidth - dur) / 2;
    const start = prevStart + offset;
    boxes.push({ layer: FLAME_LAYERS[i], ns: totals[i], start, width: dur });
    prevStart = start;
    prevWidth = dur;
  }
  return { boxes, root };
}

function flamegraphCanvas() {
  const c = document.getElementById('flamegraph');
  const dpr = window.devicePixelRatio || 1;
  const cssW = c.clientWidth;
  const cssH = parseInt(c.getAttribute('height'), 10) || 200;
  if (c.width !== cssW * dpr || c.height !== cssH * dpr) {
    c.width = cssW * dpr; c.height = cssH * dpr;
    c.style.height = cssH + 'px';
  }
  const ctx = c.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { ctx, w: cssW, h: cssH };
}

function fmtNs(ns) {
  if (!isFinite(ns) || ns <= 0) return '–';
  if (ns >= 1_000_000) return (ns / 1_000_000).toFixed(2) + ' ms';
  if (ns >= 1_000) return (ns / 1000).toFixed(1) + ' µs';
  return ns.toFixed(0) + ' ns';
}

// Compact axis labels — uPlot picks log-10 gridlines (1, 10, 100, …), so we
// round-trip through unit suffixes and drop trailing zeros to keep them short.
function fmtAxisNs(ns) {
  if (ns === null || ns === undefined || !isFinite(ns) || ns <= 0) return '–';
  if (ns >= 1_000_000) return (ns / 1_000_000) + 'ms';
  if (ns >= 1_000)     return (ns / 1_000)     + 'µs';
  return ns + 'ns';
}

function drawFlamegraph() {
  const { ctx, w, h } = flamegraphCanvas();
  ctx.clearRect(0, 0, w, h);

  if (flameRttBuffer.length === 0) {
    ctx.fillStyle = '#8b949e';
    ctx.font = '12px system-ui, sans-serif';
    ctx.textBaseline = 'top';
    ctx.fillText('waiting for fills…', 10, 10);
    return;
  }

  const { boxes, root } = flamegraphBoxes();
  const pad = 4;
  const rowH = Math.floor((h - pad * 2) / FLAME_LAYERS.length);
  const px = (ns) => (ns / root) * (w - pad * 2);

  // Bottom layer is row 0 visually; canvas y grows downward, so layer i sits
  // at y = h - pad - (i+1) * rowH.
  for (let i = 0; i < boxes.length; i++) {
    const b = boxes[i];
    const x = pad + px(b.start);
    const y = h - pad - (i + 1) * rowH;
    const bw = Math.max(1, px(b.width));
    const bh = rowH - 1;

    ctx.fillStyle = b.layer.color;
    ctx.globalAlpha = (flameHover === null || flameHover === i) ? 1.0 : 0.55;
    ctx.fillRect(x, y, bw, bh);
    ctx.globalAlpha = 1.0;

    // Label inside the box if it fits.
    const label = `${b.layer.name} · ${fmtNs(b.ns)}`;
    ctx.font = '11px ui-monospace, monospace';
    const tw = ctx.measureText(label).width;
    if (tw + 8 < bw) {
      ctx.fillStyle = '#0d1117';
      ctx.textBaseline = 'middle';
      ctx.fillText(label, x + 4, y + bh / 2);
    } else if (bw > 36) {
      // Just the name if duration won't fit.
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

function flamegraphHitTest(ev) {
  const c = document.getElementById('flamegraph');
  const rect = c.getBoundingClientRect();
  const x = ev.clientX - rect.left;
  const y = ev.clientY - rect.top;
  const h = parseInt(c.getAttribute('height'), 10) || 200;
  const pad = 4;
  const rowH = Math.floor((h - pad * 2) / FLAME_LAYERS.length);
  const { boxes, root } = flamegraphBoxes();
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

function flamegraphMove(ev) {
  const hit = flamegraphHitTest(ev);
  const tip = document.getElementById('flamegraph-tip');
  if (!hit) {
    if (flameHover !== null) { flameHover = null; drawFlamegraph(); }
    tip.style.display = 'none';
    return;
  }
  if (flameHover !== hit.idx) { flameHover = hit.idx; drawFlamegraph(); }
  const { boxes, root } = flamegraphBoxes();
  const parentNs = hit.idx === 0 ? root : (boxes[hit.idx - 1].ns || 1);
  const pctParent = (hit.box.ns / parentNs) * 100;
  const pctRoot = (hit.box.ns / (root || 1)) * 100;
  tip.innerHTML =
    `<strong>${hit.box.layer.name}</strong> — ${fmtNs(hit.box.ns)}<br>` +
    `${hit.box.layer.detail}<br>` +
    `${pctParent.toFixed(1)}% of parent · ${pctRoot.toFixed(1)}% of total`;
  const wrap = document.getElementById('flamegraph').parentElement;
  const wrapRect = wrap.getBoundingClientRect();
  const x = ev.clientX - wrapRect.left + 12;
  const y = ev.clientY - wrapRect.top + 12;
  tip.style.left = x + 'px';
  tip.style.top = y + 'px';
  tip.style.display = 'block';
}

function flamegraphLeave() {
  flameHover = null;
  document.getElementById('flamegraph-tip').style.display = 'none';
  drawFlamegraph();
}

function initFlameLegend() {
  const root = document.getElementById('flame-legend');
  root.innerHTML = FLAME_LAYERS.map(l =>
    `<div class="row"><span class="sw" style="background:${l.color}"></span>` +
    `<span class="name">${l.name}</span><span>${l.detail}</span></div>`
  ).join('');
}

// ── Order stream control ──────────────────────────────────────────────────
function startStream() {
  const rate = parseInt(document.getElementById('rate').value, 10);
  const qty = parseInt(document.getElementById('qty').value, 10);
  const sideSel = document.getElementById('side').value;
  if (ticker) clearInterval(ticker);
  ticker = setInterval(() => {
    let side;
    if (sideSel === 'alt') { side = nextSide; nextSide ^= 1; }
    else side = parseInt(sideSel, 10);
    sent += 1;
    document.getElementById('sent').textContent = sent.toLocaleString();
    tracker.send({ side, qty });
  }, Math.max(50, Math.floor(1000 / rate)));
  const btn = document.getElementById('start');
  btn.textContent = 'stop'; btn.classList.add('stop');
  btn.onclick = stopStream;
}

function stopStream() {
  if (ticker) { clearInterval(ticker); ticker = null; }
  const btn = document.getElementById('start');
  btn.textContent = 'start'; btn.classList.remove('stop');
  btn.onclick = startStream;
}

// ── Bootstrap ─────────────────────────────────────────────────────────────
window.addEventListener('DOMContentLoaded', () => {
  initChart();
  initFlameLegend();
  drawFlamegraph();

  document.getElementById('start').onclick = startStream;
  const flameEl = document.getElementById('flamegraph');
  flameEl.addEventListener('mousemove', flamegraphMove);
  flameEl.addEventListener('mouseleave', flamegraphLeave);

  const flameRO = new ResizeObserver(() => drawFlamegraph());
  flameRO.observe(flameEl);

  let pricedFills = 0;
  tracker.onResponse((rt) => {
    const fill = rt.payload;
    filled += 1;
    document.getElementById('filled').textContent = filled.toLocaleString();
    if (fill.fill_price_bps > 0 && fill.filled_qty > 0) {
      sumPx += fill.fill_price_bps;
      pricedFills += 1;
      document.getElementById('px').textContent = (sumPx / pricedFills / 10000).toFixed(5);
    }
    const rttMs = (rt.rttNs / 1_000_000).toFixed(2);
    document.getElementById('rtt').textContent = rttMs + ' ms';
    document.getElementById('rtt-meta').textContent = `last fill ${rttMs} ms · ${filled} fills`;
    updateBreakdown(rt.stamps, rt.rttNs);
    pushChartPoint(rt.stamps, rt.rttNs);
    pushFlame(rt.stamps, rt.rttNs);
  });
});

// ── Breakdown (rtt / resident / wire) ─────────────────────────────────────
// Three rolling means so the headline numbers don't jitter on every fill.
const BD_WINDOW = 32;
const bdTotal = [], bdResident = [], bdWire = [];
function pushAvg(buf, v) {
  buf.push(v);
  if (buf.length > BD_WINDOW) buf.shift();
  let s = 0; for (const x of buf) s += x;
  return s / buf.length;
}
function fmtUs(ns) {
  if (!isFinite(ns) || ns <= 0) return '—';
  if (ns >= 1_000_000) return (ns / 1_000_000).toFixed(2) + ' ms';
  return (ns / 1000).toFixed(1) + ' µs';
}
function updateBreakdown(stamps, rttNs) {
  const resident = (stamps[0] != null && stamps[8] != null) ? (stamps[8] - stamps[0]) : 0;
  const wire = Math.max(0, rttNs - resident);
  document.getElementById('bd-total').textContent    = fmtUs(pushAvg(bdTotal, rttNs));
  document.getElementById('bd-resident').textContent = fmtUs(pushAvg(bdResident, resident));
  document.getElementById('bd-wire').textContent     = fmtUs(pushAvg(bdWire, wire));
}
