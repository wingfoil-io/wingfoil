import React from "react";
import { interpolate, useCurrentFrame } from "remotion";

import { colors, fonts, syntax } from "../theme";

/**
 * The snippet, exactly as scene 2 shows it: a doc-style body with the imports,
 * `fn main` and the helper fns left off. It is the graph from the committed
 * `crates/wingfoil/examples/core/top_of_book` example, which is what produces
 * the terminal output in scenes 4 and 5.
 *
 * The first line is the argument in miniature. `market_data` returns a
 * `MarketData` impl -- a replay or a live feed -- and everything under it is
 * wired once and cannot tell which it got.
 *
 * `book` is the shared apex: both branches read that one node, and the engine
 * runs it once per cycle however many readers it has, which is the property
 * scene 3 animates.
 */
export const SNIPPET = `// The only line that differs between backtest and live.
let feed = market_data(run_mode)?.connect(&g)?;

// The apex: one node maintains the book.
let top = feed.messages.map(move |b| apply(b, &book));

// Each side moves at its own rate.
let bid = top.map(|t| t.bid).distinct();
let ask = top.map(|t| t.ask).distinct();

bid.join(&ask, quote)
    .filter_none()
    .distinct()
    .with_time()
    .for_each(print_quote);

let mut runner = g.build();
runner.run(run_mode, RunFor::Forever)?;`;

type Token = { text: string; color: string };

const KEYWORDS = new Set(["let", "mut", "fn", "use", "move", "as", "in", "if", "else"]);
const TYPES = new Set([
  "Duration",
  "RunMode",
  "RunFor",
  "NanoTime",
  "Message",
  "HistoricalFrom",
  "Cycles",
  "Forever",
  "ZERO",
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
