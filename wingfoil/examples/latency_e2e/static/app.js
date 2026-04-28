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
  ["ws_recv→ws_publish",    "server", 0, 1],
  ["ws_publish→gw_recv",    "server", 1, 2],
  ["gw_recv→gw_price",      "server", 2, 3],
  ["gw_price→fix_send",     "server", 3, 4],
  ["fix_send→fix_recv",     "server", 4, 5],
  ["fix_recv→gw_publish",   "server", 5, 6],
  ["gw_publish→ws_sub_recv", "server", 6, 7],
  ["ws_sub_recv→ws_send",   "server", 7, 8],
  ["wire RTT (out+in)",     "wire"],
];
const COLOURS = [
  "#58a6ff","#3fb950","#f0883e","#f85149",
  "#bc8cff","#79c0ff","#ff7b72","#a371f7",
  "#e3b341",
];
const MAX_POINTS = 200; // ~100 s at 2 Hz

// ── wingfoil client + latency tracker ─────────────────────────────────────
// Server uses CodecKind::Json (see ws_server.rs); the tracker drives the
// orders → fills → latency_echo loop end-to-end.

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

// Point the embedded Grafana iframe at the operator dashboard, pre-filtered
// to this session via the `$session` template variable. Assumes Grafana is
// on the same host on port 3000 (docker-compose default).
(function initGrafana() {
  const origin = `${location.protocol}//${location.hostname}:3000`;
  const url = `${origin}/d/wingfoil-latency-e2e/wingfoil-latency-end-to-end` +
              `?var-session=${tracker.sessionHex}` +
              `&kiosk=tv&theme=dark&refresh=5s&from=now-15m&to=now`;
  document.getElementById('grafana').src = url;
})();

// ── UI state ──────────────────────────────────────────────────────────────
let sent = 0, filled = 0, sumPx = 0;
let nextSide = 0;
let chart, chartData, ticker = null;

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

// ── Per-hop chart and bars ────────────────────────────────────────────────
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

function renderStages(stamps, rttTotal) {
  const values = STAGES.map(s => stageNs(s, stamps, rttTotal));
  const max = Math.max(...values);
  const root = document.getElementById('stages');
  root.innerHTML = '';
  for (let i = 0; i < STAGES.length; i++) {
    const ns = values[i];
    const pct = max > 0 ? (ns / max) * 100 : 0;
    const row = document.createElement('div');
    row.className = 'stage-bar';
    row.innerHTML = `<span class="name">${STAGES[i][0]}</span>
      <div class="bar"><div style="width:${pct}%;background:${COLOURS[i]}"></div></div>
      <span class="v">${ns.toLocaleString()}</span>`;
    root.appendChild(row);
  }
}

function pushChartPoint(stamps, rttTotal) {
  const t = chartData[0].length;
  chartData[0].push(t);
  for (let i = 0; i < STAGES.length; i++) {
    chartData[i + 1].push(stageNs(STAGES[i], stamps, rttTotal));
  }
  while (chartData[0].length > MAX_POINTS) {
    chartData.forEach(s => s.shift());
  }
  chart.setData(chartData);
}

function initChart() {
  chartData = [[], ...STAGES.map(() => [])];
  const opts = {
    width: document.getElementById('chart').clientWidth - 16,
    height: 480,
    title: 'per-hop latency (ns) — this session',
    cursor: { drag: { x: true, y: false } },
    scales: { x: { time: false }, y: { distr: 3, log: 10 } },
    series: [
      { label: 'sample' },
      ...STAGES.map(([name], i) => ({
        label: name,
        stroke: COLOURS[i],
        width: 1.5,
      })),
    ],
    axes: [{ stroke: '#8b949e' }, { stroke: '#8b949e', values: (_, vs) => vs.map(v => (v === null || v === undefined || !isFinite(v)) ? '–' : v.toLocaleString()) }],
  };
  chart = new uPlot(opts, chartData, document.getElementById('chart'));
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
  document.getElementById('start').onclick = startStream;

  let pricedFills = 0;
  tracker.onResponse((rt) => {
    const fill = rt.payload;
    filled += 1;
    document.getElementById('filled').textContent = filled.toLocaleString();
    // Skip cancels/rejects (fix_gw emits them as zero-priced zero-qty fills
    // so the round-trip counter still closes) — including them would drag
    // the displayed average toward zero.
    if (fill.fill_price_bps > 0 && fill.filled_qty > 0) {
      sumPx += fill.fill_price_bps;
      pricedFills += 1;
      document.getElementById('px').textContent = (sumPx / pricedFills / 10000).toFixed(5);
    }
    document.getElementById('rtt').textContent = (rt.rttNs / 1_000_000).toFixed(2) + ' ms';
    renderStages(rt.stamps, rt.rttNs);
    pushChartPoint(rt.stamps, rt.rttNs);
  });
});
