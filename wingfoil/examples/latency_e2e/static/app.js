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
const FLAME_ROWS  = 40;       // flame chart depth

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
const flameFills = []; // {stamps, rttTotal, t}
let baselineIdx = null; // pinned row in flame chart

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

function totalNs(stamps, rttTotal) {
  let s = 0;
  for (const stage of STAGES) s += stageNs(stage, stamps, rttTotal);
  return s;
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
      { stroke: '#8b949e', values: (_, vs) => vs.map(v => (v === null || v === undefined || !isFinite(v)) ? '–' : v.toLocaleString()) },
    ],
  };
  chart = new uPlot(opts, chartData, document.getElementById('chart'));

  const ro = new ResizeObserver(() => {
    const sz = chartSize();
    chart.setSize(sz);
  });
  ro.observe(document.getElementById('chart'));
}

// ── Flame chart (linear pipeline) ─────────────────────────────────────────
function initFlameLegend() {
  const root = document.getElementById('flame-legend');
  root.innerHTML = STAGES.map(([name], i) =>
    `<span><span class="sw" style="background:${COLOURS[i]}"></span>${name}</span>`
  ).join('');
}

function flameCanvas() {
  const c = document.getElementById('flame');
  const dpr = window.devicePixelRatio || 1;
  const cssW = c.clientWidth;
  const cssH = parseInt(c.getAttribute('height'), 10) || 320;
  if (c.width !== cssW * dpr || c.height !== cssH * dpr) {
    c.width = cssW * dpr; c.height = cssH * dpr;
    c.style.height = cssH + 'px';
  }
  const ctx = c.getContext('2d');
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  return { ctx, w: cssW, h: cssH };
}

function drawFlame() {
  const { ctx, w, h } = flameCanvas();
  ctx.clearRect(0, 0, w, h);

  if (flameFills.length === 0) {
    ctx.fillStyle = '#8b949e';
    ctx.font = '12px system-ui, sans-serif';
    ctx.fillText('waiting for fills…', 10, 20);
    return;
  }

  const baseline = baselineIdx != null && flameFills[baselineIdx]
    ? totalNs(flameFills[baselineIdx].stamps, flameFills[baselineIdx].rttTotal)
    : null;

  const widest = baseline ?? Math.max(
    1,
    ...flameFills.map(f => totalNs(f.stamps, f.rttTotal)),
  );

  const rowH = Math.max(6, Math.min(14, Math.floor((h - 4) / Math.min(FLAME_ROWS, flameFills.length))));
  const labelW = 0; // labels live in the chart legend; flame is pure visual.

  // Newest on top.
  const fills = flameFills.slice(-FLAME_ROWS).reverse();
  for (let r = 0; r < fills.length; r++) {
    const f = fills[r];
    const y = 2 + r * rowH;
    let x = labelW;
    const total = totalNs(f.stamps, f.rttTotal);
    const scale = (w - labelW - 4) / widest;

    for (let i = 0; i < STAGES.length; i++) {
      const ns = stageNs(STAGES[i], f.stamps, f.rttTotal);
      const segW = ns * scale;
      ctx.fillStyle = COLOURS[i];
      ctx.fillRect(x, y, Math.max(0, segW), rowH - 1);
      x += segW;
    }

    // Pinned baseline row gets an outline.
    const fillsAdded = flameFills.length;
    const originalIdx = fillsAdded - 1 - r;
    if (originalIdx === baselineIdx) {
      ctx.strokeStyle = '#e6edf3';
      ctx.lineWidth = 1;
      ctx.strokeRect(labelW + 0.5, y + 0.5, w - labelW - 4, rowH - 2);
    }

    // Total ns at the right edge if there's room.
    if (rowH >= 10) {
      ctx.fillStyle = 'rgba(13,17,23,0.7)';
      ctx.fillRect(w - 64, y, 60, rowH - 1);
      ctx.fillStyle = '#e6edf3';
      ctx.font = '10px ui-monospace, monospace';
      ctx.textBaseline = 'middle';
      ctx.fillText((total / 1000).toFixed(1) + ' µs', w - 60, y + (rowH - 1) / 2);
    }
  }
}

function flameClickHandler(ev) {
  if (flameFills.length === 0) return;
  const c = document.getElementById('flame');
  const rect = c.getBoundingClientRect();
  const y = ev.clientY - rect.top - 2;
  const visible = Math.min(FLAME_ROWS, flameFills.length);
  const rowH = Math.max(6, Math.min(14, Math.floor((parseInt(c.getAttribute('height'), 10) - 4) / visible)));
  const r = Math.floor(y / rowH);
  if (r < 0 || r >= visible) return;
  const idx = flameFills.length - 1 - r;
  baselineIdx = baselineIdx === idx ? null : idx;
  drawFlame();
}

function pushFlame(stamps, rttTotal) {
  flameFills.push({ stamps: stamps.slice(), rttTotal });
  // Keep memory bounded.
  if (flameFills.length > FLAME_ROWS * 4) {
    const drop = flameFills.length - FLAME_ROWS * 4;
    flameFills.splice(0, drop);
    if (baselineIdx != null) {
      baselineIdx -= drop;
      if (baselineIdx < 0) baselineIdx = null;
    }
  }
  drawFlame();
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
  drawFlame();

  document.getElementById('start').onclick = startStream;
  document.getElementById('flame').addEventListener('click', flameClickHandler);

  const flameRO = new ResizeObserver(() => drawFlame());
  flameRO.observe(document.getElementById('flame'));

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
    pushChartPoint(rt.stamps, rt.rttNs);
    pushFlame(rt.stamps, rt.rttNs);
  });
});
