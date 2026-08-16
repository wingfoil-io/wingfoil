import React from "react";
import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from "remotion";

import { colors, fonts } from "../theme";

/**
 * Scene 1 -- the muted-first hook. No caption band: the type *is* the message,
 * and it has to land before a thumb moves. The first line is legible on frame
 * one, so there is no animation to sit through.
 *
 * The two rules under "two codebases" shear apart on the drift beat: the only
 * motion in the scene, and it means something.
 */
export const Hook: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const DRIFT_AT = Math.round(1.9 * fps);
  const RESOLVE_AT = Math.round(3.3 * fps);

  const driftIn = spring({ frame: frame - DRIFT_AT, fps, config: { damping: 200 }, durationInFrames: 16 });
  const resolveIn = spring({
    frame: frame - RESOLVE_AT,
    fps,
    config: { damping: 200 },
    durationInFrames: 18,
  });

  // The shear that gives "they drift" its meaning.
  const shear = interpolate(driftIn, [0, 1], [0, 1]);

  const rule = (offset: number, tone: string): React.CSSProperties => ({
    height: 6,
    borderRadius: 3,
    background: tone,
    transform: `translateX(${offset * shear * 74}px) rotate(${offset * shear * 1.5}deg)`,
    opacity: 0.55 + 0.45 * shear,
  });

  return (
    <AbsoluteFill
      style={{
        justifyContent: "center",
        alignItems: "center",
        padding: "0 90px",
        textAlign: "center",
        fontFamily: fonts.display,
      }}
    >
      <div
        style={{
          fontSize: 84,
          fontWeight: 800,
          lineHeight: 1.1,
          letterSpacing: -2.2,
          color: colors.text,
        }}
      >
        Backtest and live:
        <br />
        two codebases.
      </div>

      <div style={{ display: "grid", gap: 14, width: 470, marginTop: 46, marginBottom: 12 }}>
        <div style={rule(-1, colors.accent)} />
        <div style={rule(1, colors.live)} />
      </div>

      <div
        style={{
          marginTop: 30,
          fontSize: 68,
          fontWeight: 800,
          letterSpacing: -1.4,
          color: colors.live,
          opacity: driftIn,
          transform: `translateY(${(1 - driftIn) * 18}px)`,
        }}
      >
        → They drift.
      </div>

      <div
        style={{
          marginTop: 44,
          fontSize: 50,
          fontWeight: 700,
          letterSpacing: -0.6,
          color: colors.accent,
          opacity: resolveIn,
          transform: `translateY(${(1 - resolveIn) * 16}px)`,
        }}
      >
        wingfoil runs one graph as both.
      </div>
    </AbsoluteFill>
  );
};
