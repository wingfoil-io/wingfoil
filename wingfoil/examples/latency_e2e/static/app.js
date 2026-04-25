// wingfoil latency end-to-end — browser client.
//
// Emits OrderFrames at the configured rate, listens for FillFrames,
// stamps a t_client_recv and posts the round-trip back as an EchoFrame.
// Renders a uPlot chart of the per-hop latency for THIS session.

// Each entry is either a server-clock delta (indices into stamps[]) or
// the derived `wire_rtt` computed from the client-clock RTT minus the
// server residence time. Every number lives inside a single clock
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

// ── session UUID as 16 raw bytes (sent verbatim as JSON array) ───────────
function newSessionId() {
  const u = (crypto.randomUUID ? crypto.randomUUID() : fallbackUuid()).replaceAll('-', '');
  const out = new Uint8Array(16);
  for (let i = 0; i < 16; i++) out[i] = parseInt(u.slice(i*2, i*2+2), 16);
  return out;
}
function fallbackUuid() {
  const r = new Uint8Array(16); crypto.getRandomValues(r);
  r[6] = (r[6] & 0x0f) | 0x40; r[8] = (r[8] & 0x3f) | 0x80;
  const h = [...r].map(b => b.toString(16).padStart(2, '0')).join('');
  return `${h.slice(0,8)}-${h.slice(8,12)}-${h.slice(12,16)}-${h.slice(16,20)}-${h.slice(20)}`;
}
function hex(bytes) { return [...bytes].map(b => b.toString(16).padStart(2,'0')).join(''); }
function nowNs() { return Math.round(performance.timeOrigin * 1e6 + performance.now() * 1e6); }

// ── WebSocket envelope plumbing ──────────────────────────────────────────
//
// CodecKind::Json on the server: each WS frame is JSON-encoded
// `Envelope { topic, time_ns, payload: Vec<u8> }`. The payload is itself
// the JSON-encoded user type as a byte array. We unwrap one level here
// so handlers see the actual object.

let ws = null;
function connect(onMsg) {
  const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`;
  ws = new WebSocket(url);
  ws.binaryType = 'arraybuffer';
  ws.onopen = () => {
    setStatus('live');
    // Subscribe to the topics we care about.
    sendCtrl({ Subscribe: { topics: ['fills', 'session'] } });
  };
  ws.onclose = () => { setStatus('disconnected'); ws = null; };
  ws.onerror = (e) => console.warn('ws error', e);
  ws.onmessage = async (ev) => {
    const buf = typeof ev.data === 'string'
      ? new TextEncoder().encode(ev.data)
      : new Uint8Array(ev.data);
    let env;
    try { env = JSON.parse(new TextDecoder().decode(buf)); }
    catch (e) { console.warn('bad envelope', e); return; }
    if (env.topic === '$ctrl') return;
    let payload;
    try { payload = JSON.parse(new TextDecoder().decode(new Uint8Array(env.payload))); }
    catch (e) { console.warn('bad payload', env.topic, e); return; }
    onMsg(env.topic, payload);
  };
}

function sendFrame(topic, payloadObj) {
  if (!ws || ws.readyState !== 1) return;
  const payloadBytes = new TextEncoder().encode(JSON.stringify(payloadObj));
  const env = { topic, time_ns: 0, payload: Array.from(payloadBytes) };
  ws.send(JSON.stringify(env));
}
function sendCtrl(ctrl) {
  const payloadBytes = new TextEncoder().encode(JSON.stringify(ctrl));
  const env = { topic: '$ctrl', time_ns: 0, payload: Array.from(payloadBytes) };
  ws.send(JSON.stringify(env));
}

// ── UI state ──────────────────────────────────────────────────────────────
const session = newSessionId();
const sessionArr = Array.from(session);
let seq = 0, sent = 0, filled = 0, sumPx = 0, lastRttNs = 0;
let nextSide = 0;
let chart, chartData, ticker = null;

document.getElementById('session').textContent = hex(session).slice(0, 8) + '…';

// Point the embedded Grafana iframe at the operator dashboard, pre-filtered
// to this session via the `$session` template variable. Assumes Grafana is
// on the same host on port 3000 (docker-compose default). `kiosk=tv` hides
// Grafana chrome; `theme=dark` matches the page.
(function initGrafana() {
  const origin = `${location.protocol}//${location.hostname}:3000`;
  const url = `${origin}/d/wingfoil-latency-e2e/wingfoil-latency-end-to-end` +
              `?var-session=${hex(session)}` +
              `&kiosk=tv&theme=dark&refresh=5s&from=now-15m&to=now`;
  document.getElementById('grafana').src = url;
})();

function setStatus(s) {
  const el = document.getElementById('status');
  el.textContent = s;
  el.className = 'pill ' + (s === 'live' ? 'live' : 'idle');
}

// Resolve one STAGES entry to a delta in ns. Server stages read from
// stamps[]; the wire stage reads rtt_total and server residence, both
// derived from same-domain subtractions.
function stageNs(entry, stamps, rttTotal) {
  const [, kind, a, b] = entry;
  if (kind === 'server') return stamps[b] - stamps[a];
  if (kind === 'wire')   return Math.max(0, rttTotal - (stamps[8] - stamps[0]));
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
    axes: [{ stroke: '#8b949e' }, { stroke: '#8b949e', values: (_, vs) => vs.map(v => v.toLocaleString()) }],
  };
  chart = new uPlot(opts, chartData, document.getElementById('chart'));
}

function startStream() {
  const rate = parseInt(document.getElementById('rate').value, 10);
  const qty = parseInt(document.getElementById('qty').value, 10);
  const sideSel = document.getElementById('side').value;
  if (ticker) clearInterval(ticker);
  ticker = setInterval(() => {
    if (!ws || ws.readyState !== 1) return;
    let side;
    if (sideSel === 'alt') { side = nextSide; nextSide ^= 1; }
    else side = parseInt(sideSel, 10);
    seq += 1; sent += 1;
    document.getElementById('sent').textContent = sent.toLocaleString();
    sendFrame('orders', {
      session: sessionArr,
      client_seq: seq,
      side, qty,
      t_client_send: nowNs(),
    });
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
  connect((topic, msg) => {
    if (topic === 'fills') {
      // Filter: only this session's fills.
      if (!sameId(msg.session, sessionArr)) return;
      const tRecv = nowNs();
      const rttTotal = Math.max(0, tRecv - msg.t_client_send);
      filled += 1;
      sumPx += msg.fill_price_bps;
      lastRttNs = rttTotal;
      document.getElementById('filled').textContent = filled.toLocaleString();
      document.getElementById('px').textContent = (sumPx / filled / 10000).toFixed(5);
      document.getElementById('rtt').textContent = (rttTotal / 1_000_000).toFixed(2) + ' ms';
      renderStages(msg.stamps, rttTotal);
      pushChartPoint(msg.stamps, rttTotal);
      // Echo to the server with t_client_recv stamped. The server
      // derives rtt_total and wire_rtt from the four stamps, all
      // arithmetic within a single clock domain per delta.
      sendFrame('latency_echo', {
        session: sessionArr,
        client_seq: msg.client_seq,
        t_client_send: msg.t_client_send,
        t_client_recv: tRecv,
        stamps: msg.stamps,
      });
    } else if (topic === 'session') {
      // Reserved — server can broadcast queue state here.
      console.log('session', msg);
    }
  });
});

function sameId(a, b) {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}
