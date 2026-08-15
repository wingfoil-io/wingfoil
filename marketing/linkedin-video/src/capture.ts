/**
 * The captured program output scenes 4 and 5 render.
 *
 * `assets/terminal.json` is written by `scripts/capture-output.sh`, which runs
 * the committed `crates/wingfoil/examples/core/top_of_book` example -- the
 * snippet from scene 2 -- once per run mode over the LOBSTER AAPL sample, and
 * records what it actually printed along with the command that printed it.
 * Nothing in this file invents a quote, and the capture script fails if the two
 * runs ever stop producing identical quotes, because that is the claim the
 * video makes.
 */

import terminal from "../assets/terminal.json";

export type Row = {
  time: string;
  bid: string;
  ask: string;
  spread: string;
  mid: string;
};

export const historical = terminal.historical as Row[];
export const realtime = terminal.realtime as Row[];

/** Quote changes over the whole run, of which the video shows the head. */
export const QUOTES = terminal.quotes;

/** The command that produced these rows, shown on the terminal's prompt line. */
export const COMMAND = terminal.command;

/** The same command in the form that selects a live run. */
export const COMMAND_REALTIME = `${terminal.command} -- realtime`;

/**
 * How long the replay actually took, median of repeated --release runs of the
 * very command on screen. This is the "instant backtest" claim as a number, and
 * it describes *this* graph over *this* data -- not a synthetic benchmark.
 */
export const replay = terminal.replay;

/** Two significant figures: the measurement does not support more. */
export const speedup = (n: number): string => {
  const rounded = Number(n.toPrecision(2));
  return rounded.toLocaleString("en-US");
};

/** The half of a row that is identical across run modes. */
export const quoteOf = (r: Row): string => `${r.bid}|${r.ask}|${r.spread}|${r.mid}`;
