// Nanosecond formatters shared by the telemetry components.
//
// These are duplicated into the future `@wingfoil/telemetry` package; the
// generic dashboard lives there. This in-tree copy exists so the example
// runs without a build step or external bundle dependency.

export function fmtNs(ns) {
  if (!isFinite(ns) || ns <= 0) return '–';
  if (ns >= 1_000_000) return (ns / 1_000_000).toFixed(2) + ' ms';
  if (ns >= 1_000) return (ns / 1000).toFixed(1) + ' µs';
  return ns.toFixed(0) + ' ns';
}

// uPlot's log-10 gridlines land on 1, 10, 100, …, so we round-trip the
// magnitude through a unit suffix and drop trailing zeros to keep the
// labels short.
export function fmtAxisNs(ns) {
  if (ns === null || ns === undefined || !isFinite(ns) || ns <= 0) return '–';
  if (ns >= 1_000_000) return (ns / 1_000_000) + 'ms';
  if (ns >= 1_000)     return (ns / 1_000)     + 'µs';
  return ns + 'ns';
}

export function fmtUs(ns) {
  if (!isFinite(ns) || ns <= 0) return '—';
  if (ns >= 1_000_000) return (ns / 1_000_000).toFixed(2) + ' ms';
  return (ns / 1000).toFixed(1) + ' µs';
}
