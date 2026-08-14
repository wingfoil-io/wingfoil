import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame } from "remotion";

import { brand, colors, fonts } from "../theme";

/**
 * The ground every scene sits on: flat dark fill, a barely-there grid, and the
 * brand's two colours blooming from opposite corners so consecutive scenes feel
 * like one film rather than seven cards.
 */
export const Backdrop: React.FC<{ seed?: number }> = ({ seed = 0 }) => {
  const frame = useCurrentFrame();
  const drift = Math.sin((frame + seed * 40) / 90) * 60;

  return (
    <AbsoluteFill style={{ background: colors.bg }}>
      <AbsoluteFill
        style={{
          backgroundImage: `linear-gradient(${colors.surfaceEdge} 1px, transparent 1px),
                            linear-gradient(90deg, ${colors.surfaceEdge} 1px, transparent 1px)`,
          backgroundSize: "60px 60px",
          opacity: 0.15,
        }}
      />
      <AbsoluteFill
        style={{
          background: `radial-gradient(660px 660px at ${300 + drift}px ${300 - drift / 2}px,
                        rgba(255, 49, 201, 0.13), transparent 70%)`,
        }}
      />
      <AbsoluteFill
        style={{
          background: `radial-gradient(660px 660px at ${790 - drift}px ${470 + drift / 2}px,
                        rgba(44, 152, 255, 0.14), transparent 70%)`,
        }}
      />
      <AbsoluteFill
        style={{
          background:
            "radial-gradient(900px 900px at 50% 120%, rgba(7, 10, 20, 0.92), transparent 60%)",
        }}
      />
    </AbsoluteFill>
  );
};

/**
 * Wraps a scene with its backdrop and a short cross-fade at each end, so cuts
 * read as transitions instead of jumps.
 */
export const Stage: React.FC<{
  durationInFrames: number;
  seed?: number;
  children: React.ReactNode;
}> = ({ durationInFrames, seed, children }) => {
  const frame = useCurrentFrame();
  const fade = interpolate(
    frame,
    [0, 5, durationInFrames - 6, durationInFrames - 1],
    [0, 1, 1, 0],
    { extrapolateLeft: "clamp", extrapolateRight: "clamp" },
  );

  return (
    <AbsoluteFill style={{ opacity: fade }}>
      <Backdrop seed={seed} />
      <AbsoluteFill>{children}</AbsoluteFill>
    </AbsoluteFill>
  );
};

/** A rule painted in the brand sweep -- the hook's two colours, resolved. */
export const GradientRule: React.FC<{ style?: React.CSSProperties }> = ({ style }) => (
  <div style={{ height: 6, borderRadius: 3, background: brand.gradient, ...style }} />
);
