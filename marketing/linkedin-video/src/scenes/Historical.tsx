import React from "react";
import { AbsoluteFill, useVideoConfig } from "remotion";

import { Terminal } from "../components/Terminal";
import { COMMAND, approx, count, historical, replay } from "../capture";
import { colors, fonts } from "../theme";

/**
 * Scene 4 -- the historical run. Every row lands within a few frames of the
 * command, because a replay does not wait for the wall clock: six ticks of
 * engine time, ten milliseconds apart, resolved as fast as the CPU can walk
 * the graph.
 *
 * Blue throughout: this is the engine's deterministic side.
 */
export const Historical: React.FC = () => {
  const { fps } = useVideoConfig();
  const start = Math.round(0.55 * fps);

  return (
    <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", paddingBottom: 160 }}>
      <Terminal
        command={COMMAND}
        rows={historical}
        // Effectively at once -- one frame apart is the smallest gap the format
        // can show, and the point is that there is no waiting.
        rowFrames={historical.map((_, i) => start + i)}
        timeColor={colors.accent}
        fontSize={26}
        width={1000}
      />

      {/* "Instant" as a number, measured on the very command above: the median
          of repeated --release runs, captured with the output. This describes
          *this* graph over *this* data — no benchmark, no workload footnote.

          Throughput is the metric this audience compares. An earlier cut said
          "6,196× faster than real time", which measured how quiet the tape was
          rather than how fast the engine is. */}
      <div
        style={{
          marginTop: 32,
          textAlign: "center",
          fontFamily: fonts.mono,
          whiteSpace: "pre",
        }}
      >
        <div style={{ fontSize: 25, color: colors.textMuted }}>
          1 hour of AAPL · {count(replay.messages)} messages ·{" "}
          {count(replay.quotes)} quotes
        </div>
        <div style={{ marginTop: 12, fontSize: 31, color: colors.textMuted }}>
          replayed in{" "}
          <span style={{ color: colors.text, fontWeight: 700 }}>
            {replay.medianMs.toFixed(0)} ms
          </span>
          {"   ·   "}
          <span style={{ color: colors.accent, fontWeight: 700 }}>
            {approx(replay.messagesPerSecond)} msg/s
          </span>
        </div>
      </div>
    </AbsoluteFill>
  );
};
