import React from "react";
import { interpolate, useCurrentFrame } from "remotion";

import { colors, fonts, syntax } from "../theme";

/**
 * The snippet, exactly as scene 2 shows it: a doc-style body with the imports
 * and `fn main` left off. It is the same graph as the committed
 * `crates/wingfoil/examples/core/odds_evens` example and as `capture/src/main.rs`,
 * which is what produces the terminal output in scenes 4 and 5.
 *
 * Chain style follows the house rule -- receiver on its own line with each
 * `.method()` indented under it, and short chains kept inline.
 */
export const SNIPPET = `let count = ticker(Duration::from_millis(10)).count();

let evens = count
    .filter(&count.map(|i| i % 2 == 0))
    .map(|i| format!("{i} is even"));
let odds = count
    .filter(&count.map(|i| i % 2 == 1))
    .map(|i| format!("{i} is odd"));

odds
    .merge(&evens)
    .logged("odds/evens", Level::Info)
    .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(6))
    .unwrap();`;

type Token = { text: string; color: string };

const KEYWORDS = new Set(["let", "mut", "fn", "use", "move", "as", "in", "if", "else"]);
const TYPES = new Set([
  "Duration",
  "RunMode",
  "RunFor",
  "NanoTime",
  "Level",
  "HistoricalFrom",
  "Cycles",
  "ZERO",
  "Info",
]);

/**
 * A deliberately small Rust tokenizer -- enough for this one snippet, with no
 * dependency to download. Order matters: strings first, then the categories
 * that would otherwise eat each other.
 */
const RULES: { re: RegExp; color: (m: string) => string }[] = [
  { re: /^"(?:[^"\\]|\\.)*"/, color: () => syntax.string },
  { re: /^\/\/[^\n]*/, color: () => syntax.punct },
  { re: /^[A-Za-z_][A-Za-z0-9_]*!/, color: () => syntax.macro },
  {
    re: /^\.[A-Za-z_][A-Za-z0-9_]*/,
    color: () => syntax.method,
  },
  {
    re: /^[A-Za-z_][A-Za-z0-9_]*/,
    color: (m) =>
      KEYWORDS.has(m) ? syntax.keyword : TYPES.has(m) ? syntax.type : syntax.plain,
  },
  { re: /^\d[\d_]*/, color: () => syntax.number },
  { re: /^\|[^|]*\|/, color: () => syntax.closure },
  { re: /^[(){}\[\];,.:&=<>%+\-*/!]+/, color: () => syntax.punct },
  { re: /^\s+/, color: () => syntax.plain },
];

const tokenize = (line: string): Token[] => {
  const out: Token[] = [];
  let rest = line;
  while (rest.length > 0) {
    const hit = RULES.map((r) => ({ r, m: r.re.exec(rest) })).find((x) => x.m);
    if (!hit || !hit.m) {
      out.push({ text: rest[0], color: syntax.plain });
      rest = rest.slice(1);
      continue;
    }
    const text = hit.m[0];
    out.push({ text, color: hit.r.color(text) });
    rest = rest.slice(text.length);
  }
  return out;
};

/**
 * Reveals the snippet a line at a time. Blank lines carry no reveal cost --
 * they appear with the line that follows them, so the rhythm tracks code
 * rather than whitespace.
 */
export const CodeBlock: React.FC<{
  code: string;
  startFrame: number;
  framesPerLine: number;
  fontSize?: number;
}> = ({ code, startFrame, framesPerLine, fontSize = 23 }) => {
  const frame = useCurrentFrame();
  const lines = code.split("\n");

  // Map each line to its reveal index, skipping blanks.
  let revealIndex = 0;
  const revealAt = lines.map((line) => (line.trim() === "" ? revealIndex : revealIndex++));

  return (
    <div
      style={{
        fontFamily: fonts.mono,
        fontSize,
        lineHeight: 1.62,
        color: colors.text,
        whiteSpace: "pre",
        textAlign: "left",
      }}
    >
      {lines.map((line, i) => {
        const at = startFrame + revealAt[i] * framesPerLine;
        const opacity = interpolate(frame, [at, at + 7], [0, 1], {
          extrapolateLeft: "clamp",
          extrapolateRight: "clamp",
        });
        const shift = interpolate(frame, [at, at + 7], [-9, 0], {
          extrapolateLeft: "clamp",
          extrapolateRight: "clamp",
        });
        return (
          <div key={i} style={{ opacity, transform: `translateX(${shift}px)`, minHeight: "1em" }}>
            {tokenize(line).map((tok, j) => (
              <span key={j} style={{ color: tok.color }}>
                {tok.text}
              </span>
            ))}
          </div>
        );
      })}
    </div>
  );
};
