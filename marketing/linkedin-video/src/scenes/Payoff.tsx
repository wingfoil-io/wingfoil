import React from "react";
import { AbsoluteFill, spring, useCurrentFrame, useVideoConfig } from "remotion";

import { colors, fonts } from "../theme";

/**
 * Scene 6 -- the payoff. The two rules that sheared apart in the hook come
 * back together and settle into one, which is the whole argument in a single
 * piece of motion.
 */
export const Payoff: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const converge = spring({ frame, fps, config: { damping: 200 }, durationInFrames: 26 });
  const second = spring({
    frame: frame - Math.round(1.7 * fps),
    fps,
    config: { damping: 200 },
    durationInFrames: 20,
  });

  const spread = (1 - converge) * 74;

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
        Same graph.
        <br />
        Same results.
      </div>

      {/* The hook's two drifting rules, brought back together -- and then made
          one, which is the argument in a single piece of motion. */}
      <div style={{ position: "relative", width: 470, height: 6, margin: "56px 0 12px" }}>
        <div
          style={{
            position: "absolute",
            inset: 0,
            height: 6,
            borderRadius: 3,
            background: colors.accent,
            transform: `translateX(${-spread}px) translateY(${-7 * (1 - converge)}px)`,
          }}
        />
        <div
          style={{
            position: "absolute",
            inset: 0,
            height: 6,
            borderRadius: 3,
            background: colors.amber,
            transform: `translateX(${spread}px) translateY(${7 * (1 - converge)}px)`,
            // Fades out as it lands on the teal rule: two become one.
            opacity: 1 - Math.max(0, (converge - 0.8) / 0.2),
          }}
        />
      </div>

      <div
        style={{
          marginTop: 46,
          fontSize: 52,
          fontWeight: 800,
          letterSpacing: -1,
          color: colors.accent,
          opacity: second,
          transform: `translateY(${(1 - second) * 18}px)`,
        }}
      >
        Backtest and live can&rsquo;t drift.
      </div>
    </AbsoluteFill>
  );
};
