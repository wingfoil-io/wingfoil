import React from "react";
import { AbsoluteFill, useVideoConfig } from "remotion";

import { CodeBlock, SNIPPET } from "../components/Code";
import { SceneTitle } from "../components/Stage";
import { colors } from "../theme";

/**
 * Scene 2 -- the snippet, revealed a line at a time. The reveal is paced to
 * finish before the voice does, so the caption band never explains code that
 * has not appeared yet.
 */
export const Code: React.FC<{ durationInFrames: number }> = ({ durationInFrames }) => {
  const { fps } = useVideoConfig();

  const start = Math.round(0.35 * fps);
  const nonBlankLines = SNIPPET.split("\n").filter((l) => l.trim() !== "").length;
  // Land the last line about a second before the scene ends.
  const framesPerLine = Math.max(
    3,
    Math.floor((durationInFrames - start - fps) / nonBlankLines),
  );

  return (
    <AbsoluteFill>
      <SceneTitle>Describe it once — in Rust.</SceneTitle>
      <AbsoluteFill
        style={{
          justifyContent: "center",
          alignItems: "center",
          paddingBottom: 150,
        }}
      >
        <div
          style={{
            background: "rgba(9, 15, 26, 0.9)",
            border: `1px solid ${colors.surfaceEdge}`,
            borderRadius: 16,
            padding: "30px 34px",
            boxShadow: "0 30px 70px rgba(0, 0, 0, 0.5)",
          }}
        >
          <CodeBlock code={SNIPPET} startFrame={start} framesPerLine={framesPerLine} />
        </div>
      </AbsoluteFill>
    </AbsoluteFill>
  );
};
