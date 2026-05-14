// Barrel module — mirrors the future `@wingfoil/telemetry` package surface.
//
// All exports here are pipeline-agnostic. Anything FIX/LMAX/iceoryx2-flavored
// belongs in the example, not the package.

export { LatencyChart } from './chart.js';
export { FlameGraph } from './flamegraph.js';
export { BreakdownPanel } from './breakdown.js';
export { StatusPill } from './status.js';
export { fmtNs, fmtAxisNs, fmtUs } from './format.js';
