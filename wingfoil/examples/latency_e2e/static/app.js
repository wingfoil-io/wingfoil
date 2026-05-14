// wingfoil latency end-to-end — example-specific browser glue.
//
// Everything pipeline-agnostic (chart, flamegraph, breakdown tiles,
// status pill, formatters) lives in `@wingfoil/telemetry` on npm.
// This file holds only the bits unique to this demo: the control plane
// for the server-side order generator, fill stats, the FIX/LMAX-flavored
// stage and layer labels, and the Grafana iframe.
//
// Order generation runs server-side now — the browser publishes one
// `ControlFrame` to start/stop or to update side/qty/rate live, and
// subscribes to fills. There's no `setInterval` and no LatencyTracker.

import { WingfoilClient } from "@wingfoil/client";
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
//
// The cross-clock `wire RTT` row is gone: with orders generated
// server-side there's no browser-clock leg to subtract `resident` from.
const STAGES = [
  { name: 'ws_recv→ws_publish',     color: '#58a6ff', value: (rt) => delta(rt, 0, 1) },
  { name: 'ws_publish→gw_recv',     color: '#3fb950', value: (rt) => delta(rt, 1, 2) },
  { name: 'gw_recv→gw_price',       color: '#f0883e', value: (rt) => delta(rt, 2, 3) },
  { name: 'gw_price→fix_send',      color: '#f85149', value: (rt) => delta(rt, 3, 4) },
  { name: 'fix_send→fix_recv',      color: '#bc8cff', value: (rt) => delta(rt, 4, 5) },
  { name: 'fix_recv→gw_publish',    color: '#79c0ff', value: (rt) => delta(rt, 5, 6) },
  { name: 'gw_publish→ws_sub_recv', color: '#ff7b72', value: (rt) => delta(rt, 6, 7) },
  { name: 'ws_sub_recv→ws_send',    color: '#a371f7', value: (rt) => delta(rt, 7, 8) },
];

// Flamegraph layers, outermost-first. Bottom is `resident` (ws_recv →
// ws_send, the whole server pipeline) — the browser-RTT row is gone
// since the browser no longer sends individual orders that round-trip
// on the client clock.
const FLAME_LAYERS = [
  { name: 'resident',  detail: 'ws_recv → ws_send (this server, end-to-end)',  color: '#3fb950', value: (rt) => delta(rt, 0, 8) },
  { name: 'iceoryx2',  detail: 'ws_publish → ws_sub_recv (shm + fix_gw)',      color: '#bc8cff', value: (rt) => delta(rt, 1, 7) },
  { name: 'fix_gw',    detail: 'gw_recv → gw_publish (this process)',          color: '#f0883e', value: (rt) => delta(rt, 2, 6) },
  { name: 'lmax',      detail: 'fix_send → fix_recv (FIX/TLS to LMAX + match)', color: '#f85149', value: (rt) => delta(rt, 4, 5) },
];

function delta(rt, a, b) {
  const A = rt.stamps[a], B = rt.stamps[b];
  if (A == null || B == null) return 0;
  return Math.max(0, B - A);
}

// ── Control-frame protocol (mirrors shared.rs) ────────────────────────────
const CONTROL_STOP = 0;
const CONTROL_START = 1;
const SIDE_ALT = 255;

// ── Session UUID ──────────────────────────────────────────────────────────
// 16 random bytes; the server keys its per-session generator on this and
// fills are broadcast on a single topic, so the listener filters here.
const sessionBytes = crypto.getRandomValues(new Uint8Array(16));
const sessionArr = Array.from(sessionBytes);
const sessionHex = sessionBytes.reduce((s, b) => s + b.toString(16).padStart(2, '0'), '');
function sameSession(a) {
  if (!a || a.length !== 16) return false;
  for (let i = 0; i < 16; i++) if (a[i] !== sessionArr[i]) return false;
  return true;
}

// ── wingfoil client ───────────────────────────────────────────────────────
const client = new WingfoilClient({
  url: `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`,
  codec: 'json',
});

document.getElementById('session').textContent = sessionHex.slice(0, 8) + '…';

// Grafana iframe + pop-out link, both pre-filtered to this session.
(function initGrafana() {
  const origin = `${location.protocol}//${location.hostname}:3000`;
  const url = `${origin}/d/wingfoil-latency-e2e/wingfoil-latency-end-to-end` +
              `?var-session=${sessionHex}` +
              `&kiosk=tv&theme=dark&refresh=5s&from=now-15m&to=now`;
  document.getElementById('grafana').src = url;
  document.getElementById('grafana-pop').href = url;
})();

// ── Control plane ─────────────────────────────────────────────────────────
let running = false;
let filled = 0, sumPx = 0, pricedFills = 0;

function readControls() {
  const sideSel = document.getElementById('side').value;
  const side = sideSel === 'alt' ? SIDE_ALT : parseInt(sideSel, 10);
  const qty = Math.max(1, parseInt(document.getElementById('qty').value, 10) || 1);
  const rate_hz = Math.max(1, parseInt(document.getElementById('rate').value, 10) || 1);
  return { side, qty, rate_hz };
}

function sendControl(action) {
  const { side, qty, rate_hz } = readControls();
  client.publish('control', {
    session: sessionArr,
    action,
    side,
    qty,
    rate_hz,
  });
}

function startStream() {
  sendControl(CONTROL_START);
  running = true;
  const btn = document.getElementById('start');
  btn.textContent = 'stop'; btn.classList.add('stop');
  btn.onclick = stopStream;
}

function stopStream() {
  sendControl(CONTROL_STOP);
  running = false;
  const btn = document.getElementById('start');
  btn.textContent = 'start'; btn.classList.remove('stop');
  btn.onclick = startStream;
}

// Live-update path: any change while running re-publishes CONTROL_START
// so the server updates the active generator in place.
function controlsChanged() {
  if (running) sendControl(CONTROL_START);
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

  // Only the resident bucket survives — total/wire required a browser-clock
  // leg that no longer exists. The shared component still takes a list.
  const breakdown = new BreakdownPanel({
    buckets: [
      { el: document.getElementById('bd-resident'), value: (rt) => rt.serverResidentNs },
    ],
  });

  document.getElementById('start').onclick = startStream;
  for (const id of ['side', 'qty', 'rate']) {
    document.getElementById(id).addEventListener('change', controlsChanged);
  }

  client.subscribe('fills', (raw) => {
    const msg = raw || {};
    if (!sameSession(msg.session)) return;
    const stamps = msg.stamps || [];
    const serverResidentNs = (stamps[0] != null && stamps[8] != null)
      ? Math.max(0, stamps[8] - stamps[0])
      : 0;
    const rt = { payload: msg, stamps, serverResidentNs };

    filled += 1;
    document.getElementById('filled').textContent = filled.toLocaleString();
    if (msg.fill_price_bps > 0 && msg.filled_qty > 0) {
      sumPx += msg.fill_price_bps;
      pricedFills += 1;
      document.getElementById('px').textContent = (sumPx / pricedFills / 10000).toFixed(5);
    }
    const residentMs = (serverResidentNs / 1_000_000).toFixed(2);
    document.getElementById('rtt').textContent = residentMs + ' ms';
    document.getElementById('rtt-meta').textContent = `last resident ${residentMs} ms · ${filled} fills`;

    chart.push(rt);
    flame.push(rt);
    breakdown.push(rt);
  });

  // Best-effort stop on tab close so the server-side generator doesn't
  // keep firing until TTL expires.
  window.addEventListener('beforeunload', () => {
    if (running) sendControl(CONTROL_STOP);
  });
});
