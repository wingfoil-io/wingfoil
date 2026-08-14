import React from "react";
import { AbsoluteFill, useCurrentFrame, useVideoConfig } from "remotion";

import { Badge } from "../components/Stage";
import { Terminal } from "../components/Terminal";
import { COMMAND_REALTIME, historical, realtime } from "../capture";
import { colors } from "../theme";

/** The blinking prompt between arrivals -- the wall clock being waited on. */
const Waiting: React.FC<{ done: boolean }> = ({ done }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  if (done) {
    return null;
  }
  const pulse = 0.35 + 0.45 * (0.5 + 0.5 * Math.sin((frame / fps) * 6.5));
  return <div style={{ color: colors.live, opacity: pulse }}>waiting…</div>;
};

/**
 * Scene 5 -- the same graph, live.
 *
 * The rows are the *captured realtime run*, not the historical rows re-timed:
 * live engine time is the wall clock, so the timestamps are epoch nanoseconds
 * and the historical run's are not. That difference is left visible and dimmed
 * in magenta -- the brand's "live" colour -- while the value column, identical
 * to scene 4 character for character, is the half held bright. Same graph, same
 * values, different clock.
 */
export const Realtime: React.FC<{ durationInFrames: number }> = ({ durationInFrames }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const start = Math.round(0.55 * fps);
  const gap = Math.max(6, Math.floor((durationInFrames - start - fps * 1.25) / realtime.length));
  const rowFrames = realtime.map((_, i) => start + i * gap);
  const done = frame >= rowFrames[rowFrames.length - 1];

  // Guard the claim in the pixels, not just in the capture script.
  if (realtime.map((r) => r.value).join("|") !== historical.map((r) => r.value).join("|")) {
    throw new Error("captured values differ across run modes — re-run scripts/capture-output.sh");
  }

  return (
    <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", paddingBottom: 180 }}>
      <Terminal
        command={COMMAND_REALTIME}
        rows={realtime}
        rowFrames={rowFrames}
        timeColor={colors.live}
        fontSize={22}
        footer={<Waiting done={done} />}
      />
      <div style={{ marginTop: 36 }}>
        <Badge tone="live" delay={start + 8}>
          ⏱ live · wall-clock · same values, same order
        </Badge>
      </div>
    </AbsoluteFill>
  );
};
