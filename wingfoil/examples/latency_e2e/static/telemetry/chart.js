// Per-hop latency chart — uPlot wrapper.
//
// Pipeline-agnostic: callers declare each stage as
//   { name, color, value: (rt) => number }
// where `rt` is whatever shape the caller passes to `push()`. For the
// wingfoil `LatencyTracker`, `rt` is a `RoundTrip` from
// `@wingfoil/client/tracing`.

import { fmtAxisNs } from './format.js';

export class LatencyChart {
  constructor({ host, stages, maxPoints = 200 }) {
    if (!host) throw new Error('LatencyChart: host element required');
    if (!Array.isArray(stages) || stages.length === 0) {
      throw new Error('LatencyChart: stages must be a non-empty array');
    }
    this.host = host;
    this.stages = stages;
    this.maxPoints = maxPoints;
    this._data = [[], ...stages.map(() => [])];
    this._init();
  }

  _size() {
    const w = Math.max(280, this.host.clientWidth);
    const h = Math.max(220, Math.min(420, Math.round(w * 0.42)));
    return { width: w, height: h };
  }

  _init() {
    const { width, height } = this._size();
    const opts = {
      width, height,
      cursor: { drag: { x: true, y: false } },
      scales: { x: { time: false }, y: { distr: 3, log: 10 } },
      legend: { show: true },
      series: [
        { label: 'sample' },
        ...this.stages.map((s) => ({
          label: s.name,
          stroke: s.color,
          width: 1.5,
        })),
      ],
      axes: [
        { stroke: '#8b949e' },
        { stroke: '#8b949e', size: 64, values: (_, vs) => vs.map(fmtAxisNs) },
      ],
    };
    // uPlot is loaded as a global IIFE on the page.
    this._chart = new uPlot(opts, this._data, this.host);

    this._ro = new ResizeObserver(() => this._chart.setSize(this._size()));
    this._ro.observe(this.host);
  }

  push(rt) {
    const d = this._data;
    d[0].push(d[0].length);
    for (let i = 0; i < this.stages.length; i++) {
      d[i + 1].push(this.stages[i].value(rt));
    }
    while (d[0].length > this.maxPoints) d.forEach((s) => s.shift());
    // Rewrite x as a contiguous index after shifting.
    d[0] = d[0].map((_, i) => i);
    this._chart.setData(d);
  }

  destroy() {
    this._ro?.disconnect();
    this._chart?.destroy();
  }
}
