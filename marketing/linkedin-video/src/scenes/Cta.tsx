import React from "react";
import { AbsoluteFill, Img, interpolate, spring, staticFile, useCurrentFrame, useVideoConfig } from "remotion";

import { brand, colors, fonts } from "../theme";

/**
 * Scene 7 -- the card the viewer leaves on.
 *
 * The mark is the real logo (`public/brand/wingfoil-mark.png`, from the
 * `wingfoil-io/assets` deck), not a redraw: a hexagonal spiral in the same
 * magenta-to-blue sweep the payoff's rule just resolved into. The repo link
 * lives in the first comment on the post, not on screen, so the CTA points
 * there.
 */
export const Cta: React.FC = () => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const mark = spring({ frame, fps, config: { damping: 200 }, durationInFrames: 26 });
  const word = spring({ frame: frame - 10, fps, config: { damping: 200 }, durationInFrames: 22 });
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
  const bloom = interpolate(mark, [0, 1], [0, 0.5]);

  return (
    <AbsoluteFill
      style={{
        justifyContent: "center",
        alignItems: "center",
        textAlign: "center",
        fontFamily: fonts.display,
      }}
    >
      <div
        style={{
          position: "relative",
          marginBottom: 30,
          opacity: mark,
          transform: `scale(${0.86 + 0.14 * mark})`,
        }}
      >
        {/* The mark's own colours, bloomed behind it. */}
        <div
          style={{
            position: "absolute",
            inset: -70,
            background:
              "radial-gradient(circle, rgba(255,49,201,0.30), rgba(44,152,255,0.18) 55%, transparent 72%)",
            opacity: bloom,
          }}
        />
        <Img src={staticFile("brand/wingfoil-mark.png")} style={{ width: 250, height: 250 }} />
      </div>

      <div
        style={{
          fontSize: 112,
          fontWeight: 800,
          letterSpacing: -3.6,
          background: brand.gradient,
          WebkitBackgroundClip: "text",
          backgroundClip: "text",
          color: "transparent",
          opacity: word,
          transform: `translateY(${(1 - word) * 16}px)`,
        }}
      >
        wingfoil
      </div>

      <div
        style={{
          marginTop: 6,
          fontSize: 40,
          fontWeight: 600,
          letterSpacing: 0.6,
          color: colors.text,
          opacity: tag,
        }}
      >
        Stream processing in Rust
      </div>

      <div
        style={{
          marginTop: 66,
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
