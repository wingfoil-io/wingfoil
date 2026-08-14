import React from "react";
import { AbsoluteFill, useVideoConfig } from "remotion";

import { Badge, SceneTitle } from "../components/Stage";
import { Terminal } from "../components/Terminal";
import { COMMAND, historical } from "../capture";
import { colors } from "../theme";

/**
 * Scene 4 -- the historical run. Every row lands within a few frames of the
 * command, because a replay does not wait for the wall clock: six ticks of
 * engine time, ten milliseconds apart, resolved as fast as the CPU can walk
 * the graph.
 */
export const Historical: React.FC = () => {
  const { fps } = useVideoConfig();
  const start = Math.round(0.55 * fps);

  return (
    <AbsoluteFill>
      <SceneTitle>Run it as a backtest.</SceneTitle>
      <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", paddingBottom: 120 }}>
        <Terminal
          command={COMMAND}
          rows={historical}
          // Effectively at once -- one frame apart is the smallest gap the
          // format can show, and the point is that there is no waiting.
          rowFrames={historical.map((_, i) => start + i)}
          timeColor={colors.accent}
        />
        <div style={{ marginTop: 34 }}>
          <Badge tone="accent" delay={start + 8}>
            ⚡ backtest · instant · deterministic
          </Badge>
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};
