/**
 * The captured program output scenes 4 and 5 render.
 *
 * `assets/terminal.json` is written by `scripts/capture-output.sh`, which runs
 * the committed `crates/wingfoil/examples/core/odds_evens` example -- the
 * snippet from scene 2 -- once per run mode and records what it actually
 * printed, along with the command that printed it. Nothing in this file invents
 * a line, and the capture script fails if the two runs ever stop producing
 * identical values, because that is the claim the video makes.
 */

import terminal from "../assets/terminal.json";

export type Row = { time: string; label: string; value: string };

export const historical = terminal.historical as Row[];
export const realtime = terminal.realtime as Row[];

/** The command that produced these rows, shown on the terminal's prompt line. */
export const COMMAND = terminal.command;

/** The same command in the form that selects a live run. */
export const COMMAND_REALTIME = `${terminal.command} -- realtime`;
