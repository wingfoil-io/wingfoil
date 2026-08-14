import React from "react";
import { AbsoluteFill, interpolate, spring, useCurrentFrame, useVideoConfig } from "remotion";

import { colors, fonts } from "../theme";

/**
 * The wordmark's glyph: the diamond from scene 3, drawn down to a mark. It
 * traces itself in, so the logo is built out of the same idea the video is
 * about rather than being decoration.
 */
const Mark: React.FC<{ progress: number }> = ({ progress }) => {
  const seg = (i: number) => interpolate(progress, [i * 0.16, i * 0.16 + 0.4], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });

  const edges: [number, number, number, number][] = [
    [50, 12, 14, 50],
    [50, 12, 86, 50],
    [14, 50, 50, 88],
    [86, 50, 50, 88],
  ];

  return (
    <svg width={106} height={106} viewBox="0 0 100 100">
      {edges.map(([x1, y1, x2, y2], i) => (
        <line
          key={i}
          x1={x1}
          y1={y1}
          x2={x2}
          y2={y2}
          stroke={colors.accent}
          strokeWidth={4}
          strokeLinecap="round"
          pathLength={1}
          strokeDasharray={1}
          strokeDashoffset={1 - seg(i)}
        />
      ))}
      {([
        [50, 12],
        [14, 50],
        [86, 50],
        [50, 88],
      ] as const).map(([cx, cy], i) => (
        <circle
          key={i}
          cx={cx}
          cy={cy}
          r={7}
          fill={colors.bg}
          stroke={colors.accentSoft}
          strokeWidth={4}
          opacity={seg(Math.max(0, i - 1))}
        />
      ))}
    </svg>
  );
};

/**
 * Scene 7 -- the card the viewer leaves on. The repo link lives in the first
 * comment on the post, not on screen, so the CTA points there.
 */
export const Cta: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const draw = interpolate(frame, [0, Math.round(1.3 * fps)], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const word = spring({ frame: frame - 8, fps, config: { damping: 200 }, durationInFrames: 22 });
  const tag = spring({
    frame: frame - Math.round(0.95 * fps),
    fps,
    config: { damping: 200 },
    durationInFrames: 20,
  });
  const cta = spring({
    frame: frame - Math.round(1.9 * fps),
    fps,
    config: { damping: 200 },
    durationInFrames: 20,
  });
  const nudge = Math.sin((frame / fps) * 4.2) * 5 * cta;

  return (
    <AbsoluteFill
      style={{
        justifyContent: "center",
        alignItems: "center",
        textAlign: "center",
        fontFamily: fonts.display,
      }}
    >
      <div style={{ opacity: draw > 0.02 ? 1 : 0, marginBottom: 26 }}>
        <Mark progress={draw} />
      </div>

      <div
        style={{
          fontSize: 108,
          fontWeight: 800,
          letterSpacing: -3.4,
          color: colors.text,
          opacity: word,
          transform: `translateY(${(1 - word) * 16}px)`,
        }}
      >
        wingfoil
      </div>

      <div
        style={{
          marginTop: 10,
          fontSize: 40,
          fontWeight: 600,
          letterSpacing: 0.6,
          color: colors.accent,
          opacity: tag,
        }}
      >
        Stream processing in Rust
      </div>

      <div
        style={{
          marginTop: 74,
          fontSize: 36,
          fontWeight: 700,
          color: colors.textMuted,
          opacity: cta,
          transform: `translateY(${nudge}px)`,
        }}
      >
        Code in the comments 👇
      </div>
    </AbsoluteFill>
  );
};
