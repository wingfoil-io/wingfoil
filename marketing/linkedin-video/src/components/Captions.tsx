import React from "react";
import { useCurrentFrame, useVideoConfig } from "remotion";

import { caption, colors, fonts } from "../theme";
import type { Word } from "../narration";

/**
 * Burned-in karaoke captions -- the primary channel, not a courtesy. Most of
 * this video's audience watches it muted in a feed, so the band renders one
 * sentence at a time with the word currently being spoken lifted out of it.
 *
 * Geometry is fixed by `theme.caption` and deliberately conservative: a capped
 * width with word-level wrapping, so a long word can never push the text off
 * the safe area on a phone.
 */
export const Captions: React.FC<{
  words: Word[];
  sentences: { text: string; start: number; end: number }[];
}> = ({ words, sentences }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();
  const t = frame / fps;

  // Hold the last sentence through trailing silence rather than flashing empty.
  const active =
    sentences.find((s) => t >= s.start && t <= s.end + 0.45) ??
    (t > (sentences.at(-1)?.end ?? 0) ? sentences.at(-1) : undefined);
  if (!active) {
    return null;
  }

  const spoken = words.filter((w) => w.start >= active.start - 1e-6 && w.end <= active.end + 1e-6);

  return (
    <div
      style={{
        position: "absolute",
        left: 0,
        right: 0,
        bottom: caption.bottom,
        display: "flex",
        justifyContent: "center",
        padding: "0 40px",
      }}
    >
      <div
        style={{
          maxWidth: caption.maxWidth,
          background: "rgba(6, 11, 20, 0.82)",
          border: `1px solid ${colors.surfaceEdge}`,
          borderRadius: 18,
          padding: "18px 26px",
          textAlign: "center",
          fontFamily: fonts.display,
          fontSize: caption.fontSize,
          fontWeight: 600,
          lineHeight: 1.36,
          letterSpacing: 0.1,
          color: colors.textMuted,
          overflowWrap: "break-word",
          wordBreak: "normal",
        }}
      >
        {spoken.map((w, i) => {
          const on = t >= w.start && t < w.end;
          const said = t >= w.end;
          return (
            <span
              key={i}
              style={{
                color: on ? colors.accent : said ? colors.text : colors.textMuted,
                transition: "none",
              }}
            >
              {w.word}
              {i === spoken.length - 1 ? "" : " "}
            </span>
          );
        })}
      </div>
    </div>
  );
};
