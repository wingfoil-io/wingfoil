# `@wingfoil/telemetry` — package scaffold handoff

This document is a complete, self-contained scaffold for the
**closed-source** `@wingfoil/telemetry` package, intended to be
materialized into the empty `wingfoil-io/wingfoil-telemetry-js`
repository.

## Background

The five UI components below are carved out of the `latency_e2e`
example in this repo (see PR #344, branch
`claude/refactor-e2e-ui-components-PtvXJ`). They were ported from the
in-tree JS modules at
`wingfoil/examples/latency_e2e/static/telemetry/*.js` to TypeScript,
verified with `tsc --noEmit` and 23 unit tests under `vitest` running
on `happy-dom`. The package is published to npm under the
`@wingfoil` scope as public-read; the source remains private.

This handoff doc exists because Claude Code on the web is single-repo
per session — a session targeting `wingfoil-io/wingfoil` cannot also
push commits to `wingfoil-io/wingfoil-telemetry-js`. The workflow is
therefore: an agent in a *separate* session targeting the empty
`wingfoil-telemetry-js` repo reads this doc and materializes the
files.

## Instructions for the receiving agent

1. Confirm you are in a session targeting `wingfoil-io/wingfoil-telemetry-js`.
2. For each `## File: <path>` heading below, create the file at that
   path inside the repository working tree, using the content inside the
   four-backtick fence verbatim.
3. Do not invent or modify anything. If any file appears truncated or
   inconsistent, stop and report rather than guessing.
4. After all 15 files exist:
   - Run `pnpm install` (or `npm install`).
   - Run `pnpm run lint` — must pass.
   - Run `pnpm test` — must report 23 passed.
   - Run `pnpm run build` — must produce `dist/` with `.js`, `.d.ts`,
     `.js.map`, `.d.ts.map` for `breakdown`, `chart`, `flamegraph`,
     `format`, `index`, `status`.
5. Commit with message:
   ```
   initial commit — scaffold pipeline-agnostic telemetry dashboard package
   ```
   and a body describing the five components and that 23 unit tests pass.
   Push to `main`.

## Design summary

Five modules, all generic over a `T` round-trip type and consuming
caller-supplied value-extractor functions so nothing about FIX,
market data, or stamp-index layout is baked in:

| Module | Class | Purpose |
|--------|-------|---------|
| `chart`      | `LatencyChart<T>`    | uPlot per-stage line chart (log Y). |
| `flamegraph` | `FlameGraph<T>`      | Nested-call-stack canvas with rolling-mean boxes. |
| `breakdown`  | `BreakdownPanel<T>`  | N rolling-mean tiles. |
| `status`     | `StatusPill`         | Connection pill bound to a `WingfoilClient`-shaped object. |
| `format`     | `fmtNs / fmtAxisNs / fmtUs` | Nanosecond formatters. |

`uplot` is a (peer) dependency for `chart`; the other modules have no
runtime deps. Structural typing on `status` so there's no hard dep on
`@wingfoil/client`.

---

## File: `.gitignore`

````
node_modules/
dist/
.vite/
*.tsbuildinfo
coverage/
````

## File: `package.json`

````json
{
  "name": "@wingfoil/telemetry",
  "version": "0.1.0",
  "description": "Pipeline-agnostic latency-telemetry dashboard components (chart, flamegraph, breakdown, status pill) for the wingfoil web stack.",
  "license": "UNLICENSED",
  "private": false,
  "homepage": "https://github.com/wingfoil-io/wingfoil-telemetry-js",
  "repository": {
    "type": "git",
    "url": "git+https://github.com/wingfoil-io/wingfoil-telemetry-js.git"
  },
  "type": "module",
  "main": "./dist/index.js",
  "module": "./dist/index.js",
  "types": "./dist/index.d.ts",
  "exports": {
    ".": {
      "types": "./dist/index.d.ts",
      "import": "./dist/index.js"
    },
    "./chart": {
      "types": "./dist/chart.d.ts",
      "import": "./dist/chart.js"
    },
    "./flamegraph": {
      "types": "./dist/flamegraph.d.ts",
      "import": "./dist/flamegraph.js"
    },
    "./breakdown": {
      "types": "./dist/breakdown.d.ts",
      "import": "./dist/breakdown.js"
    },
    "./status": {
      "types": "./dist/status.d.ts",
      "import": "./dist/status.js"
    },
    "./format": {
      "types": "./dist/format.d.ts",
      "import": "./dist/format.js"
    }
  },
  "files": [
    "dist",
    "README.md"
  ],
  "scripts": {
    "build": "tsc",
    "lint": "tsc --noEmit",
    "test": "vitest run",
    "test:watch": "vitest"
  },
  "peerDependencies": {
    "uplot": "^1.6.0"
  },
  "peerDependenciesMeta": {
    "uplot": {
      "optional": true
    }
  },
  "devDependencies": {
    "@types/node": "^22.0.0",
    "happy-dom": "^15.0.0",
    "typescript": "^5.5.0",
    "uplot": "^1.6.31",
    "vitest": "^3.2.4"
  }
}
````

## File: `tsconfig.json`

````json
{
  "compilerOptions": {
    "target": "ES2022",
    "module": "ESNext",
    "moduleResolution": "Bundler",
    "lib": ["ES2022", "DOM", "DOM.Iterable"],
    "strict": true,
    "esModuleInterop": true,
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true,
    "declaration": true,
    "declarationMap": true,
    "sourceMap": true,
    "outDir": "dist",
    "rootDir": "src"
  },
  "include": ["src/**/*"],
  "exclude": ["node_modules", "dist", "tests"]
}
````

## File: `vitest.config.ts`

````ts
import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "happy-dom",
    include: ["tests/**/*.test.ts"],
  },
});
````

## File: `README.md`

````md
# @wingfoil/telemetry

Pipeline-agnostic latency-telemetry dashboard components for the
wingfoil web stack. Each component takes a host element plus a
declarative spec and exposes a `push(rt)` / `destroy()` lifecycle —
nothing about FIX, market data, or stamp-index layout is baked in.

Designed to pair with [`@wingfoil/client`](https://www.npmjs.com/package/@wingfoil/client)'s
`LatencyTracker` (which emits `RoundTrip<T>` events), but the components
are generic over `T` — pass whichever shape your server produces and
provide value-extractor functions in the spec.

## Components

| Module | Class | Purpose |
|--------|-------|---------|
| `@wingfoil/telemetry/chart`      | `LatencyChart`    | Per-stage line chart over a log Y-axis (wraps uPlot). |
| `@wingfoil/telemetry/flamegraph` | `FlameGraph`      | Nested-call-stack canvas with rolling-mean boxes and hover tooltip. |
| `@wingfoil/telemetry/breakdown`  | `BreakdownPanel`  | N rolling-mean tiles (e.g. total / resident / wire). |
| `@wingfoil/telemetry/status`     | `StatusPill`      | Connection-status pill bound to a `WingfoilClient`. |
| `@wingfoil/telemetry/format`     | `fmtNs`, `fmtAxisNs`, `fmtUs` | Nanosecond formatters. |

All five are re-exported from the package root.

## Example

```ts
import { WingfoilClient } from "@wingfoil/client";
import { LatencyTracker, RoundTrip } from "@wingfoil/client/tracing";
import {
  LatencyChart,
  FlameGraph,
  BreakdownPanel,
  StatusPill,
} from "@wingfoil/telemetry";

const client = new WingfoilClient({ url: "wss://your-host/ws", codec: "json" });
const tracker = new LatencyTracker({ client, outbound: "in", inbound: "out", echo: "echo" });

new StatusPill({ host: document.getElementById("status")!, client });

const chart = new LatencyChart<RoundTrip>({
  host: document.getElementById("chart")!,
  stages: [
    { name: "stage 0 → 1", color: "#58a6ff", value: (rt) => rt.stamps[1] - rt.stamps[0] },
    { name: "wire RTT",    color: "#e3b341", value: (rt) => rt.wireRttNs },
  ],
});

const flame = new FlameGraph<RoundTrip>({
  host: document.getElementById("flamegraph") as HTMLCanvasElement,
  layers: [
    { name: "browser", color: "#58a6ff", detail: "full RTT",       value: (rt) => rt.rttNs },
    { name: "server",  color: "#3fb950", detail: "stamps[0..last]", value: (rt) => rt.serverResidentNs },
  ],
});

const breakdown = new BreakdownPanel<RoundTrip>({
  buckets: [
    { el: document.getElementById("total")!,    value: (rt) => rt.rttNs },
    { el: document.getElementById("resident")!, value: (rt) => rt.serverResidentNs },
    { el: document.getElementById("wire")!,     value: (rt) => rt.wireRttNs },
  ],
});

tracker.onResponse((rt) => {
  chart.push(rt);
  flame.push(rt);
  breakdown.push(rt);
});
```

Reference deployment: the `latency_e2e` example in the
[wingfoil](https://github.com/wingfoil-io/wingfoil) repo.

## Peer dependencies

- **`uplot ^1.6`** — required for the `chart` module. The other modules
  have no peer deps.

## Build

```bash
npm install
npm run build      # tsc → dist/
npm test           # vitest, happy-dom environment
```
````

## File: `src/format.ts`

````ts
// Nanosecond formatters.

export function fmtNs(ns: number): string {
  if (!isFinite(ns) || ns <= 0) return "–";
  if (ns >= 1_000_000) return (ns / 1_000_000).toFixed(2) + " ms";
  if (ns >= 1_000) return (ns / 1000).toFixed(1) + " µs";
  return ns.toFixed(0) + " ns";
}

// uPlot's log-10 gridlines land on 1, 10, 100, …, so we round-trip the
// magnitude through a unit suffix and drop trailing zeros to keep the
// labels short.
export function fmtAxisNs(ns: number | null | undefined): string {
  if (ns === null || ns === undefined || !isFinite(ns) || ns <= 0) return "–";
  if (ns >= 1_000_000) return (ns / 1_000_000) + "ms";
  if (ns >= 1_000) return (ns / 1_000) + "µs";
  return ns + "ns";
}

export function fmtUs(ns: number): string {
  if (!isFinite(ns) || ns <= 0) return "—";
  if (ns >= 1_000_000) return (ns / 1_000_000).toFixed(2) + " ms";
  return (ns / 1000).toFixed(1) + " µs";
}
````

## File: `src/chart.ts`

````ts
// Per-hop latency chart — uPlot wrapper.
//
// Pipeline-agnostic: callers declare each stage as
// `{ name, color, value: (rt: T) => number }`. `T` is the shape of
// whatever the caller passes to `push()` — typically a `RoundTrip` from
// `@wingfoil/client/tracing`, but any object works.

import uPlot, { type Options as UPlotOptions } from "uplot";
import { fmtAxisNs } from "./format.js";

export interface ChartStage<T> {
  name: string;
  color: string;
  value: (rt: T) => number;
}

export interface LatencyChartOptions<T> {
  host: HTMLElement;
  stages: ChartStage<T>[];
  /** Max points retained on the chart's rolling buffer. Defaults to 200. */
  maxPoints?: number;
}

type ChartData = [number[], ...number[][]];

export class LatencyChart<T> {
  readonly host: HTMLElement;
  readonly stages: ChartStage<T>[];
  readonly maxPoints: number;

  private _data: ChartData;
  private _chart!: uPlot;
  private _ro!: ResizeObserver;

  constructor(opts: LatencyChartOptions<T>) {
    if (!opts.host) throw new Error("LatencyChart: host element required");
    if (!Array.isArray(opts.stages) || opts.stages.length === 0) {
      throw new Error("LatencyChart: stages must be a non-empty array");
    }
    this.host = opts.host;
    this.stages = opts.stages;
    this.maxPoints = opts.maxPoints ?? 200;
    this._data = [[], ...opts.stages.map(() => [] as number[])] as ChartData;
    this._init();
  }

  push(rt: T): void {
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

  destroy(): void {
    this._ro?.disconnect();
    this._chart?.destroy();
  }

  private _size(): { width: number; height: number } {
    const w = Math.max(280, this.host.clientWidth);
    const h = Math.max(220, Math.min(420, Math.round(w * 0.42)));
    return { width: w, height: h };
  }

  private _init(): void {
    const { width, height } = this._size();
    const opts: UPlotOptions = {
      width,
      height,
      cursor: { drag: { x: true, y: false } },
      scales: { x: { time: false }, y: { distr: 3, log: 10 } },
      legend: { show: true },
      series: [
        { label: "sample" },
        ...this.stages.map((s) => ({
          label: s.name,
          stroke: s.color,
          width: 1.5,
        })),
      ],
      axes: [
        { stroke: "#8b949e" },
        { stroke: "#8b949e", size: 64, values: (_, vs) => vs.map(fmtAxisNs) },
      ],
    };
    this._chart = new uPlot(opts, this._data, this.host);

    this._ro = new ResizeObserver(() => this._chart.setSize(this._size()));
    this._ro.observe(this.host);
  }
}
````

## File: `src/flamegraph.ts`

````ts
// Nested-call-stack flamegraph rendered to <canvas>.
//
// Layers are ordered outermost-first. Layer 0 is the root (widest box);
// each subsequent layer is drawn as a strict sub-interval of the layer
// below it — same shape as a CPU flamegraph, but the samples are
// wall-clock fills. Layer durations are rolling means over the last
// `window` samples so the picture is stable across fills.

import { fmtNs } from "./format.js";

const CSS_HEIGHT = 200;

export interface FlameLayer<T> {
  name: string;
  detail: string;
  color: string;
  value: (rt: T) => number;
}

export interface FlameGraphOptions<T> {
  host: HTMLCanvasElement;
  layers: FlameLayer<T>[];
  /** Absolutely-positioned hover tooltip element. */
  tipEl?: HTMLElement | null;
  /** Sample-count badge ("n=32"). */
  metaEl?: HTMLElement | null;
  /** Container for per-layer legend rows. */
  legendHost?: HTMLElement | null;
  /** Rolling-mean window. Defaults to 32. */
  window?: number;
}

export interface FlameBox<T> {
  layer: FlameLayer<T>;
  ns: number;
  start: number;
  width: number;
}

/**
 * Pure geometry: from per-layer rolling means, compute each box's
 * `start` and `width` along the root's coordinate space. Exported so
 * tests can verify the layout without a real DOM.
 */
export function computeFlameBoxes<T>(
  layers: FlameLayer<T>[],
  totals: number[],
): { boxes: FlameBox<T>[]; root: number } {
  const root = totals[0] || 1;
  const boxes: FlameBox<T>[] = [];
  let prevStart = 0;
  let prevWidth = root;
  for (let i = 0; i < layers.length; i++) {
    const dur = Math.min(totals[i], prevWidth);
    const offset = (prevWidth - dur) / 2;
    const start = prevStart + offset;
    boxes.push({ layer: layers[i], ns: totals[i], start, width: dur });
    prevStart = start;
    prevWidth = dur;
  }
  return { boxes, root };
}

export function rollingMean(buf: number[]): number {
  if (buf.length === 0) return 0;
  let s = 0;
  for (const v of buf) s += v;
  return s / buf.length;
}

export class FlameGraph<T> {
  readonly host: HTMLCanvasElement;
  readonly layers: FlameLayer<T>[];
  readonly window: number;
  readonly tipEl: HTMLElement | null;
  readonly metaEl: HTMLElement | null;
  readonly legendHost: HTMLElement | null;

  private _buffers: number[][];
  private _hover: number | null = null;
  private _ro!: ResizeObserver;
  private readonly _onMove: (ev: MouseEvent) => void;
  private readonly _onLeave: () => void;

  constructor(opts: FlameGraphOptions<T>) {
    if (!opts.host) throw new Error("FlameGraph: host <canvas> required");
    if (!Array.isArray(opts.layers) || opts.layers.length === 0) {
      throw new Error("FlameGraph: layers must be a non-empty array");
    }
    this.host = opts.host;
    this.layers = opts.layers;
    this.tipEl = opts.tipEl ?? null;
    this.metaEl = opts.metaEl ?? null;
    this.legendHost = opts.legendHost ?? null;
    this.window = opts.window ?? 32;
    this._buffers = opts.layers.map(() => []);

    this._onMove = this._handleMove.bind(this);
    this._onLeave = this._handleLeave.bind(this);
    this.host.addEventListener("mousemove", this._onMove);
    this.host.addEventListener("mouseleave", this._onLeave);
    this._ro = new ResizeObserver(() => this._draw());
    this._ro.observe(this.host);

    this._renderLegend();
    this._draw();
  }

  push(rt: T): void {
    for (let i = 0; i < this.layers.length; i++) {
      const buf = this._buffers[i];
      buf.push(this.layers[i].value(rt));
      if (buf.length > this.window) buf.shift();
    }
    if (this.metaEl) this.metaEl.textContent = `n=${this._buffers[0].length}`;
    this._draw();
  }

  destroy(): void {
    this._ro?.disconnect();
    this.host.removeEventListener("mousemove", this._onMove);
    this.host.removeEventListener("mouseleave", this._onLeave);
  }

  // ── internals ──────────────────────────────────────────────────────────

  private _boxes(): { boxes: FlameBox<T>[]; root: number } {
    const totals = this.layers.map((_, i) => rollingMean(this._buffers[i]));
    return computeFlameBoxes(this.layers, totals);
  }

  private _canvas(): { ctx: CanvasRenderingContext2D; w: number; h: number } {
    const c = this.host;
    const dpr = window.devicePixelRatio || 1;
    const cssW = c.clientWidth;
    // Don't read `c.height` back: setting it reflects into the attribute,
    // so on the second render cssH would be the previous bitmap height
    // (e.g. 400 at dpr=2), then 800, then 1600 — the canvas grows every
    // frame and the panel flickers. Keep CSS height as a constant.
    const cssH = CSS_HEIGHT;
    if (c.width !== cssW * dpr || c.height !== cssH * dpr) {
      c.width = cssW * dpr;
      c.height = cssH * dpr;
      c.style.height = cssH + "px";
    }
    const ctx = c.getContext("2d")!;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    return { ctx, w: cssW, h: cssH };
  }

  private _draw(): void {
    const { ctx, w, h } = this._canvas();
    ctx.clearRect(0, 0, w, h);

    if (this._buffers[0].length === 0) {
      ctx.fillStyle = "#8b949e";
      ctx.font = "12px system-ui, sans-serif";
      ctx.textBaseline = "top";
      ctx.fillText("waiting for samples…", 10, 10);
      return;
    }

    const { boxes, root } = this._boxes();
    const pad = 4;
    const rowH = Math.floor((h - pad * 2) / this.layers.length);
    const px = (ns: number): number => (ns / root) * (w - pad * 2);

    // Canvas y grows downward; layer 0 sits at the bottom row.
    for (let i = 0; i < boxes.length; i++) {
      const b = boxes[i];
      const x = pad + px(b.start);
      const y = h - pad - (i + 1) * rowH;
      const bw = Math.max(1, px(b.width));
      const bh = rowH - 1;

      ctx.fillStyle = b.layer.color;
      ctx.globalAlpha = this._hover === null || this._hover === i ? 1.0 : 0.55;
      ctx.fillRect(x, y, bw, bh);
      ctx.globalAlpha = 1.0;

      const label = `${b.layer.name} · ${fmtNs(b.ns)}`;
      ctx.font = "11px ui-monospace, monospace";
      const tw = ctx.measureText(label).width;
      if (tw + 8 < bw) {
        ctx.fillStyle = "#0d1117";
        ctx.textBaseline = "middle";
        ctx.fillText(label, x + 4, y + bh / 2);
      } else if (bw > 36) {
        const short = b.layer.name;
        const stw = ctx.measureText(short).width;
        if (stw + 6 < bw) {
          ctx.fillStyle = "#0d1117";
          ctx.textBaseline = "middle";
          ctx.fillText(short, x + 3, y + bh / 2);
        }
      }
    }
  }

  private _hitTest(ev: MouseEvent): { idx: number; box: FlameBox<T> } | null {
    const c = this.host;
    const rect = c.getBoundingClientRect();
    const x = ev.clientX - rect.left;
    const y = ev.clientY - rect.top;
    const h = CSS_HEIGHT;
    const pad = 4;
    const rowH = Math.floor((h - pad * 2) / this.layers.length);
    const { boxes, root } = this._boxes();
    const w = c.clientWidth;
    const px = (ns: number): number => (ns / root) * (w - pad * 2);
    for (let i = boxes.length - 1; i >= 0; i--) {
      const b = boxes[i];
      const bx = pad + px(b.start);
      const by = h - pad - (i + 1) * rowH;
      const bw = Math.max(1, px(b.width));
      const bh = rowH - 1;
      if (x >= bx && x <= bx + bw && y >= by && y <= by + bh) {
        return { idx: i, box: b };
      }
    }
    return null;
  }

  private _handleMove(ev: MouseEvent): void {
    const hit = this._hitTest(ev);
    if (!hit) {
      if (this._hover !== null) {
        this._hover = null;
        this._draw();
      }
      if (this.tipEl) this.tipEl.style.display = "none";
      return;
    }
    if (this._hover !== hit.idx) {
      this._hover = hit.idx;
      this._draw();
    }
    if (!this.tipEl) return;
    const { boxes, root } = this._boxes();
    const parentNs = hit.idx === 0 ? root : boxes[hit.idx - 1].ns || 1;
    const pctParent = (hit.box.ns / parentNs) * 100;
    const pctRoot = (hit.box.ns / (root || 1)) * 100;
    this.tipEl.innerHTML =
      `<strong>${hit.box.layer.name}</strong> — ${fmtNs(hit.box.ns)}<br>` +
      `${hit.box.layer.detail}<br>` +
      `${pctParent.toFixed(1)}% of parent · ${pctRoot.toFixed(1)}% of total`;
    const wrap = this.host.parentElement;
    if (!wrap) return;
    const wrapRect = wrap.getBoundingClientRect();
    this.tipEl.style.left = ev.clientX - wrapRect.left + 12 + "px";
    this.tipEl.style.top = ev.clientY - wrapRect.top + 12 + "px";
    this.tipEl.style.display = "block";
  }

  private _handleLeave(): void {
    this._hover = null;
    if (this.tipEl) this.tipEl.style.display = "none";
    this._draw();
  }

  private _renderLegend(): void {
    if (!this.legendHost) return;
    this.legendHost.innerHTML = this.layers
      .map(
        (l) =>
          `<div class="row"><span class="sw" style="background:${l.color}"></span>` +
          `<span class="name">${l.name}</span><span>${l.detail ?? ""}</span></div>`,
      )
      .join("");
  }
}
````

## File: `src/breakdown.ts`

````ts
// Rolling-mean breakdown tiles.
//
// Each bucket: { el, value: (rt) => number, format?: (ns) => string }
// The mean is over the last `window` samples so the headline numbers
// don't jitter on every fill.

import { fmtUs } from "./format.js";

export interface BreakdownBucket<T> {
  el: HTMLElement;
  value: (rt: T) => number;
  /** Defaults to `fmtUs`. */
  format?: (ns: number) => string;
}

export interface BreakdownPanelOptions<T> {
  buckets: BreakdownBucket<T>[];
  /** Rolling-mean window. Defaults to 32. */
  window?: number;
}

interface InternalBucket<T> {
  el: HTMLElement;
  value: (rt: T) => number;
  format: (ns: number) => string;
  buf: number[];
}

export class BreakdownPanel<T> {
  readonly window: number;
  private _buckets: InternalBucket<T>[];

  constructor(opts: BreakdownPanelOptions<T>) {
    if (!Array.isArray(opts.buckets) || opts.buckets.length === 0) {
      throw new Error("BreakdownPanel: buckets must be a non-empty array");
    }
    this.window = opts.window ?? 32;
    this._buckets = opts.buckets.map((b) => ({
      el: b.el,
      value: b.value,
      format: b.format ?? fmtUs,
      buf: [] as number[],
    }));
  }

  push(rt: T): void {
    for (const b of this._buckets) {
      b.buf.push(b.value(rt));
      if (b.buf.length > this.window) b.buf.shift();
      let s = 0;
      for (const v of b.buf) s += v;
      b.el.textContent = b.format(s / b.buf.length);
    }
  }

  destroy(): void {}
}
````

## File: `src/status.ts`

````ts
// Connection status pill bound to a WingfoilClient.
//
// The host element receives one of three CSS classes — `live`,
// `connecting`, `idle` — alongside its base `pill` class, and its text
// content is set to the current state.
//
// Uses structural typing for the client so this module doesn't carry
// a hard dependency on `@wingfoil/client`. Anything with a matching
// `onConnection` shape works.

export type ConnectionStateKind = "open" | "connecting" | "closed" | "error";

export interface ConnectionLike {
  onConnection(listener: (state: { kind: ConnectionStateKind } & object) => void): () => void;
}

export interface StatusPillOptions {
  host: HTMLElement;
  /** A `WingfoilClient` (or structurally compatible object). Optional. */
  client?: ConnectionLike;
}

export class StatusPill {
  readonly host: HTMLElement;
  private _unbind?: () => void;

  constructor(opts: StatusPillOptions) {
    if (!opts.host) throw new Error("StatusPill: host element required");
    this.host = opts.host;
    this._set("disconnected", "idle");
    if (opts.client) {
      this._unbind = opts.client.onConnection((s) => {
        if (s.kind === "open") this._set("live", "live");
        else if (s.kind === "connecting") this._set("connecting", "connecting");
        else this._set("disconnected", "idle");
      });
    }
  }

  destroy(): void {
    this._unbind?.();
  }

  private _set(label: string, cls: string): void {
    this.host.textContent = label;
    this.host.className = "pill " + cls;
  }
}
````

## File: `src/index.ts`

````ts
// @wingfoil/telemetry — pipeline-agnostic latency-telemetry dashboard
// components for the wingfoil web stack.

export { LatencyChart } from "./chart.js";
export type { ChartStage, LatencyChartOptions } from "./chart.js";

export { FlameGraph, computeFlameBoxes, rollingMean } from "./flamegraph.js";
export type { FlameLayer, FlameGraphOptions, FlameBox } from "./flamegraph.js";

export { BreakdownPanel } from "./breakdown.js";
export type { BreakdownBucket, BreakdownPanelOptions } from "./breakdown.js";

export { StatusPill } from "./status.js";
export type { ConnectionLike, ConnectionStateKind, StatusPillOptions } from "./status.js";

export { fmtNs, fmtAxisNs, fmtUs } from "./format.js";
````

## File: `tests/format.test.ts`

````ts
import { describe, expect, it } from "vitest";
import { fmtNs, fmtAxisNs, fmtUs } from "../src/format.js";

describe("fmtNs", () => {
  it("emits ns under 1 µs", () => {
    expect(fmtNs(7)).toBe("7 ns");
    expect(fmtNs(999)).toBe("999 ns");
  });
  it("emits µs from 1 µs to 1 ms", () => {
    expect(fmtNs(1_000)).toBe("1.0 µs");
    expect(fmtNs(12_345)).toBe("12.3 µs");
  });
  it("emits ms above 1 ms", () => {
    expect(fmtNs(1_000_000)).toBe("1.00 ms");
    expect(fmtNs(1_234_567)).toBe("1.23 ms");
  });
  it("returns en-dash for zero/negative/non-finite", () => {
    expect(fmtNs(0)).toBe("–");
    expect(fmtNs(-5)).toBe("–");
    expect(fmtNs(Number.NaN)).toBe("–");
    expect(fmtNs(Number.POSITIVE_INFINITY)).toBe("–");
  });
});

describe("fmtAxisNs", () => {
  it("drops trailing zeros for axis labels", () => {
    expect(fmtAxisNs(1)).toBe("1ns");
    expect(fmtAxisNs(1000)).toBe("1µs");
    expect(fmtAxisNs(1_000_000)).toBe("1ms");
  });
  it("returns en-dash for null/undefined/non-positive", () => {
    expect(fmtAxisNs(null)).toBe("–");
    expect(fmtAxisNs(undefined)).toBe("–");
    expect(fmtAxisNs(0)).toBe("–");
    expect(fmtAxisNs(-1)).toBe("–");
  });
});

describe("fmtUs", () => {
  it("emits µs under 1 ms", () => {
    expect(fmtUs(1_000)).toBe("1.0 µs");
    expect(fmtUs(123_456)).toBe("123.5 µs");
  });
  it("emits ms above 1 ms", () => {
    expect(fmtUs(1_000_000)).toBe("1.00 ms");
  });
  it("returns em-dash for zero/negative/non-finite", () => {
    expect(fmtUs(0)).toBe("—");
    expect(fmtUs(Number.NaN)).toBe("—");
  });
});
````

## File: `tests/flamegraph.test.ts`

````ts
import { describe, expect, it } from "vitest";
import {
  computeFlameBoxes,
  rollingMean,
  type FlameLayer,
} from "../src/flamegraph.js";

interface Sample {
  v: number;
}

function layers(...names: string[]): FlameLayer<Sample>[] {
  return names.map((name) => ({
    name,
    detail: name,
    color: "#000",
    value: (s) => s.v,
  }));
}

describe("rollingMean", () => {
  it("returns 0 for empty buffer", () => {
    expect(rollingMean([])).toBe(0);
  });
  it("averages all entries", () => {
    expect(rollingMean([10, 20, 30])).toBe(20);
    expect(rollingMean([1, 1, 1, 1])).toBe(1);
  });
});

describe("computeFlameBoxes", () => {
  it("treats layer 0 as the root, layer 0 box spans [0, root]", () => {
    const { boxes, root } = computeFlameBoxes(layers("a", "b"), [100, 60]);
    expect(root).toBe(100);
    expect(boxes[0].start).toBe(0);
    expect(boxes[0].width).toBe(100);
  });

  it("centers each child within its parent", () => {
    const { boxes } = computeFlameBoxes(layers("a", "b", "c"), [100, 60, 20]);
    // layer 1: width 60, centered inside [0,100] → start = 20
    expect(boxes[1].start).toBe(20);
    expect(boxes[1].width).toBe(60);
    // layer 2: width 20, centered inside [20,80] → start = 40
    expect(boxes[2].start).toBe(40);
    expect(boxes[2].width).toBe(20);
  });

  it("clamps a child wider than its parent down to the parent width", () => {
    // Different clock domains: child measured ns can exceed parent.
    const { boxes } = computeFlameBoxes(layers("a", "b"), [100, 500]);
    expect(boxes[1].width).toBe(100);
    expect(boxes[1].start).toBe(0);
    // The raw `ns` is preserved for the tooltip even though width is clamped.
    expect(boxes[1].ns).toBe(500);
  });

  it("falls back to root = 1 when layer 0 mean is zero", () => {
    const { root } = computeFlameBoxes(layers("a", "b"), [0, 0]);
    expect(root).toBe(1);
  });

  it("preserves raw `ns` per layer even when widths are clamped", () => {
    const { boxes } = computeFlameBoxes(layers("a", "b", "c"), [100, 60, 999]);
    expect(boxes[0].ns).toBe(100);
    expect(boxes[1].ns).toBe(60);
    expect(boxes[2].ns).toBe(999);
    expect(boxes[2].width).toBe(60); // clamped to parent
  });
});
````

## File: `tests/breakdown.test.ts`

````ts
import { beforeEach, describe, expect, it } from "vitest";
import { BreakdownPanel } from "../src/breakdown.js";

interface Sample {
  rttNs: number;
  resident: number;
}

let total: HTMLElement;
let resident: HTMLElement;

beforeEach(() => {
  document.body.innerHTML = '<span id="total"></span><span id="resident"></span>';
  total = document.getElementById("total")!;
  resident = document.getElementById("resident")!;
});

describe("BreakdownPanel", () => {
  it("rejects empty bucket arrays", () => {
    expect(() => new BreakdownPanel({ buckets: [] })).toThrow();
  });

  it("renders the running mean per bucket using the default formatter", () => {
    const panel = new BreakdownPanel<Sample>({
      buckets: [
        { el: total, value: (s) => s.rttNs },
        { el: resident, value: (s) => s.resident },
      ],
    });
    panel.push({ rttNs: 2_000, resident: 1_000 });
    panel.push({ rttNs: 4_000, resident: 2_000 });
    // means: 3 µs and 1.5 µs → fmtUs default
    expect(total.textContent).toBe("3.0 µs");
    expect(resident.textContent).toBe("1.5 µs");
  });

  it("honors a per-bucket custom formatter", () => {
    const panel = new BreakdownPanel<Sample>({
      buckets: [{ el: total, value: (s) => s.rttNs, format: (n) => `${n}ns` }],
    });
    panel.push({ rttNs: 1234, resident: 0 });
    expect(total.textContent).toBe("1234ns");
  });

  it("only averages over the last `window` samples", () => {
    const panel = new BreakdownPanel<Sample>({
      buckets: [{ el: total, value: (s) => s.rttNs }],
      window: 2,
    });
    panel.push({ rttNs: 1_000, resident: 0 });
    panel.push({ rttNs: 3_000, resident: 0 }); // mean over [1k, 3k] = 2 µs
    panel.push({ rttNs: 5_000, resident: 0 }); // mean over [3k, 5k] = 4 µs
    expect(total.textContent).toBe("4.0 µs");
  });
});
````

## File: `tests/status.test.ts`

````ts
import { beforeEach, describe, expect, it } from "vitest";
import { StatusPill, type ConnectionLike, type ConnectionStateKind } from "../src/status.js";

class FakeClient implements ConnectionLike {
  private _listener: ((s: { kind: ConnectionStateKind }) => void) | null = null;
  public unsubscribed = false;
  onConnection(listener: (s: { kind: ConnectionStateKind }) => void): () => void {
    this._listener = listener;
    return () => {
      this.unsubscribed = true;
    };
  }
  emit(kind: ConnectionStateKind): void {
    this._listener?.({ kind });
  }
}

let host: HTMLElement;

beforeEach(() => {
  document.body.innerHTML = '<span id="status"></span>';
  host = document.getElementById("status")!;
});

describe("StatusPill", () => {
  it("starts in the disconnected/idle state", () => {
    new StatusPill({ host });
    expect(host.textContent).toBe("disconnected");
    expect(host.className).toBe("pill idle");
  });

  it("updates when the client reports state", () => {
    const client = new FakeClient();
    new StatusPill({ host, client });
    client.emit("connecting");
    expect(host.textContent).toBe("connecting");
    expect(host.className).toBe("pill connecting");
    client.emit("open");
    expect(host.textContent).toBe("live");
    expect(host.className).toBe("pill live");
    client.emit("closed");
    expect(host.textContent).toBe("disconnected");
    expect(host.className).toBe("pill idle");
  });

  it("unsubscribes on destroy()", () => {
    const client = new FakeClient();
    const pill = new StatusPill({ host, client });
    pill.destroy();
    expect(client.unsubscribed).toBe(true);
  });
});
````
