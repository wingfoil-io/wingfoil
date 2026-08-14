import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame } from "remotion";

import { colors, fonts } from "../theme";

/**
 * The ground every scene sits on: flat dark fill, a barely-there grid, and a
 * soft teal bloom drifting behind the content so consecutive scenes feel like
 * one film rather than seven cards.
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
          opacity: 0.16,
        }}
      />
      <AbsoluteFill
        style={{
          background: `radial-gradient(620px 620px at ${540 + drift}px ${360 - drift / 2}px,
                        rgba(34, 211, 238, 0.14), transparent 70%)`,
        }}
      />
      <AbsoluteFill
        style={{
          background:
            "radial-gradient(900px 900px at 50% 120%, rgba(8, 13, 22, 0.9), transparent 60%)",
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

/** The small eyebrow title scenes 2-5 share. */
export const SceneTitle: React.FC<{ children: React.ReactNode; delay?: number }> = ({
  children,
  delay = 0,
}) => {
  const frame = useCurrentFrame();
  const rise = interpolate(frame, [delay, delay + 12], [16, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const opacity = interpolate(frame, [delay, delay + 12], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  return (
    <div
      style={{
        position: "absolute",
        top: 74,
        left: 0,
        right: 0,
        textAlign: "center",
        fontFamily: fonts.display,
        fontSize: 40,
        fontWeight: 700,
        letterSpacing: -0.4,
        color: colors.text,
        transform: `translateY(${rise}px)`,
        opacity,
      }}
    >
      {children}
    </div>
  );
};

/** The pill under scenes 4 and 5 that names the run mode. */
export const Badge: React.FC<{
  children: React.ReactNode;
  tone: "accent" | "amber";
  delay?: number;
}> = ({ children, tone, delay = 0 }) => {
  const frame = useCurrentFrame();
  const colour = tone === "accent" ? colors.accent : colors.amber;
  const opacity = interpolate(frame, [delay, delay + 10], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const rise = interpolate(frame, [delay, delay + 10], [10, 0], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  return (
    <div
      style={{
        display: "flex",
        justifyContent: "center",
        opacity,
        transform: `translateY(${rise}px)`,
      }}
    >
      <div
        style={{
          fontFamily: fonts.display,
          fontSize: 27,
          fontWeight: 600,
          letterSpacing: 0.2,
          color: colour,
          background: `${colour}14`,
          border: `1px solid ${colour}55`,
          borderRadius: 999,
          padding: "10px 24px",
          whiteSpace: "nowrap",
        }}
      >
        {children}
      </div>
    </div>
  );
};
