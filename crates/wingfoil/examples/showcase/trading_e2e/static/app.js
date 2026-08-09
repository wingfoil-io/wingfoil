// wingfoil latency end-to-end — example-specific browser glue.
//
// Everything pipeline-agnostic (chart, flamegraph, breakdown tiles,
// status pill, formatters) lives in `@wingfoil/telemetry` on npm.
// This file holds only the bits unique to this demo: order entry,
// fill stats, the FIX/LMAX-flavored stage and layer labels, and
// the Grafana iframe.

import { WingfoilClient } from "@wingfoil/client";
import { LatencyTracker } from "@wingfoil/client/tracing";
import {
  LatencyChart,
  FlameGraph,
  BreakdownPanel,
  StatusPill,
} from "@wingfoil/telemetry";

// ── Pipeline-specific stage and layer labels ──────────────────────────────
// Server-clock indices into the nine wingfoil stamp points:
//   ws_recv → ws_publish → gw_recv → gw_price → fix_send → fix_recv →
//   gw_publish → ws_sub_recv → ws_send
const STAGES = [
  { name: 'ws_recv→ws_publish',     color: '#58a6ff', value: (rt) => delta(rt, 0, 1) },
  { name: 'ws_publish→gw_recv',     color: '#3fb950', value: (rt) => delta(rt, 1, 2) },
  { name: 'gw_recv→gw_price',       color: '#f0883e', value: (rt) => delta(rt, 2, 3) },
  { name: 'gw_price→fix_send',      color: '#f85149', value: (rt) => delta(rt, 3, 4) },
  { name: 'fix_send→fix_recv',      color: '#bc8cff', value: (rt) => delta(rt, 4, 5) },
  { name: 'fix_recv→gw_publish',    color: '#79c0ff', value: (rt) => delta(rt, 5, 6) },
  { name: 'gw_publish→ws_sub_recv', color: '#ff7b72', value: (rt) => delta(rt, 6, 7) },
  { name: 'ws_sub_recv→ws_send',    color: '#a371f7', value: (rt) => delta(rt, 7, 8) },
  { name: 'wire RTT (out+in)',      color: '#e3b341', value: (rt) => rt.wireRttNs },
];

// Flamegraph layers, outermost-first. Each is a strict sub-interval of
// the layer below it — that nesting is what makes this a flamegraph
// rather than a waterfall.
const FLAME_LAYERS = [
  { name: 'browser',   detail: 'browser → ws_server → browser (full RTT)',     color: '#58a6ff', value: (rt) => rt.rttNs },
  { name: 'ws_server', detail: 'ws_recv → ws_send (this server, end-to-end)',  color: '#3fb950', value: (rt) => delta(rt, 0, 8) },
  { name: 'iceoryx2',  detail: 'ws_publish → ws_sub_recv (shm + fix_gw)',      color: '#bc8cff', value: (rt) => delta(rt, 1, 7) },
  { name: 'fix_gw',    detail: 'gw_recv → gw_publish (this process)',          color: '#f0883e', value: (rt) => delta(rt, 2, 6) },
  { name: 'lmax',      detail: 'fix_send → fix_recv (FIX/TLS to LMAX + match)', color: '#f85149', value: (rt) => delta(rt, 4, 5) },
];

function delta(rt, a, b) {
  const A = rt.stamps[a], B = rt.stamps[b];
  if (A == null || B == null) return 0;
  return Math.max(0, B - A);
}

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

// ── Order-entry state ─────────────────────────────────────────────────────
let sent = 0, filled = 0, sumPx = 0, pricedFills = 0;
let nextSide = 0;
let ticker = null;

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
  new StatusPill({ host: document.getElementById('status'), client });

  const chart = new LatencyChart({
    host: document.getElementById('chart'),
    stages: STAGES,
  });

  const flame = new FlameGraph({
    host: document.getElementById('flamegraph'),
    layers: FLAME_LAYERS,
    tipEl: document.getElementById('flamegraph-tip'),
    metaEl: document.getElementById('flamegraph-meta'),
    legendHost: document.getElementById('flame-legend'),
  });

  const breakdown = new BreakdownPanel({
    buckets: [
      { el: document.getElementById('bd-total'),    value: (rt) => rt.rttNs },
      { el: document.getElementById('bd-resident'), value: (rt) => rt.serverResidentNs },
      { el: document.getElementById('bd-wire'),     value: (rt) => rt.wireRttNs },
    ],
  });

  document.getElementById('start').onclick = startStream;

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

    chart.push(rt);
    flame.push(rt);
    breakdown.push(rt);
  });
});
