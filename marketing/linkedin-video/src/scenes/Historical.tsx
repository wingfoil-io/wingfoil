import React from "react";
import { AbsoluteFill, useVideoConfig } from "remotion";

import { Terminal } from "../components/Terminal";
import { COMMAND, historical } from "../capture";
import { colors } from "../theme";

/**
 * Scene 4 -- the historical run. Every row lands within a few frames of the
 * command, because a replay does not wait for the wall clock: engine time is
 * pure logic, so the book updates resolve as fast as the CPU can walk the
 * graph.
 *
 * Blue throughout: this is the engine's deterministic side.
 *
 * **No timing on screen, deliberately.** Measuring this replay measures the
 * `lobster` order book (~56 ms) and stdout (~80 ms) far more than it measures
 * wingfoil (~23 ms) -- swapping `println!` for a buffered writer moved the
 * "engine" number by 30%, which is the tell. The claim the film makes is about
 * determinism, and that one it can actually support. See the README.
 */
export const Historical: React.FC = () => {
  const { fps } = useVideoConfig();
  const start = Math.round(0.55 * fps);

  return (
    <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", paddingBottom: 130 }}>
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
    </AbsoluteFill>
  );
};
