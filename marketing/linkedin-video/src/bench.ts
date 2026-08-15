/**
 * The measured tier timings scene 3 renders.
 *
 * `assets/bench.json` is written by `scripts/capture-bench.sh`, which runs the
 * committed `fanout` group from `crates/wingfoil/benches/tiers.rs` and reads
 * criterion's own estimates. Nothing here is typed by hand, and re-measuring on
 * a quieter machine is one command.
 *
 * Only wingfoil's own tiers appear. There is no third-party comparison: a ratio
 * against another library only means something with its workload attached, and
 * a marketing frame cannot carry that much fine print honestly.
 */

import bench from "../assets/bench.json";

export type Tier = {
  nsPerIteration: number;
  nsPerCycle: number;
  nsPerNodeCycle: number;
};

export const workload = bench.workload;
export const nodes = bench.nodes;
export const tiers = bench.tiers as Record<string, Tier>;
export const compiledSpeedup = bench.compiledSpeedup;

/** Render a nanosecond figure at a sensible unit. */
export const ns = (v: number): string =>
  v >= 1000 ? `${(v / 1000).toFixed(2)} µs` : `${v.toFixed(1)} ns`;
