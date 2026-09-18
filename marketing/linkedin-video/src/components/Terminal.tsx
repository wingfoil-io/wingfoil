import React from "react";
import { interpolate, useCurrentFrame } from "remotion";

import { colors, fonts } from "../theme";
import type { Row } from "../capture";

/**
 * The terminal frame shared by scenes 4 and 5. Both scenes render the *same*
 * component over captured rows from the *same* graph, differing only in when
 * each row is allowed to appear -- which is the point being made: one graph,
 * two pacings.
 *
 * The quote is rendered bright and the engine-time column dim, so the eye
 * compares the halves that matter. Across the two scenes the quotes are
 * identical and the clock is not.
 */
export const Terminal: React.FC<{
  command: string;
  rows: Row[];
  /** Frame at which each row appears, one per row. */
  rowFrames: number[];
  /** Rendered after the last visible row -- the "waiting..." pulse in scene 5. */
  footer?: React.ReactNode;
  timeColor?: string;
  fontSize?: number;
  width?: number;
}> = ({
  command,
  rows,
  rowFrames,
  footer,
  timeColor = colors.accent,
  fontSize = 26,
  width = 1000,
}) => {
  const frame = useCurrentFrame();
  const timeWidth = Math.max(...rows.map((r) => r.time.length));

  const field = (label: string, value: string, width: number) => (
    <>
      <span style={{ color: colors.textDim }}>{`  ${label} `}</span>
      <span style={{ color: colors.text, fontWeight: 700 }}>{value.padStart(width)}</span>
    </>
  );

  return (
    <div
      style={{
        width,
        background: "rgba(9, 14, 26, 0.94)",
        border: `1px solid ${colors.surfaceEdge}`,
        borderRadius: 16,
        overflow: "hidden",
        boxShadow: "0 30px 70px rgba(0, 0, 0, 0.55)",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 9,
          padding: "13px 18px",
          background: colors.bgLift,
          borderBottom: `1px solid ${colors.surfaceEdge}`,
        }}
      >
        {["#FF5F57", "#FEBC2E", "#28C840"].map((c) => (
          <div key={c} style={{ width: 12, height: 12, borderRadius: 999, background: c }} />
        ))}
        <div
          style={{ marginLeft: 10, fontFamily: fonts.mono, fontSize: 18, color: colors.textDim }}
        >
          top_of_book — AAPL
        </div>
      </div>

      <div
        style={{
          padding: "20px 24px 24px",
          fontFamily: fonts.mono,
          fontSize,
          lineHeight: 1.62,
          whiteSpace: "pre",
        }}
      >
        <div style={{ color: colors.textMuted }}>
          <span style={{ color: colors.green }}>$ </span>
          {command}
        </div>

        {/* The output area reserves its full height up front, so rows arriving
            one at a time do not shove the whole window around -- but only the
            rows that have arrived are laid out, so a footer sits directly under
            the last one rather than below a block of empty space. */}
        <div style={{ minHeight: rows.length * fontSize * 1.62 }}>
          {rows.map((row, i) => {
            const at = rowFrames[i];
            if (frame < at) {
              return null;
            }
            const opacity = interpolate(frame, [at, at + 4], [0, 1], {
              extrapolateLeft: "clamp",
              extrapolateRight: "clamp",
            });
            const shift = interpolate(frame, [at, at + 5], [7, 0], {
              extrapolateLeft: "clamp",
              extrapolateRight: "clamp",
            });
            return (
              <div key={i} style={{ opacity, transform: `translateY(${shift}px)` }}>
                <span style={{ color: timeColor }}>{row.time.padStart(timeWidth)}</span>
                {field("bid", row.bid, 6)}
                {field("ask", row.ask, 6)}
                {field("spread", row.spread, 4)}
                {field("mid", row.mid, 6)}
              </div>
            );
          })}

          {footer}
        </div>
      </div>
    </div>
  );
};
