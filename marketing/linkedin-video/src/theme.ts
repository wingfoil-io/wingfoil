/**
 * The wingfoil brand, applied to the film.
 *
 * Source: `wingfoil-io/assets` (the Sept 2025 talk deck) -- the hexagonal-spiral
 * mark in `public/brand/wingfoil-mark.png` is that deck's logo, and the two
 * anchors below are sampled from its gradient. wingfoil.io itself is blocked by
 * this sandbox's egress proxy, so if the site has moved on, re-sample here and
 * every scene follows.
 *
 * The gradient carries the argument. **Magenta is live** -- the wall clock, the
 * drift, the thing that moves. **Blue is the engine** -- the graph, the
 * deterministic replay, the thing that doesn't. They start apart in the hook and
 * end as one gradient rule in the payoff, which is the same magenta-to-blue
 * sweep the logo is drawn in.
 */

export const brand = {
  magenta: "#FF31C9",
  blue: "#2C98FF",
  /** Where the two meet, sampled mid-sweep across the mark. */
  indigo: "#5D78FF",
  gradient: "linear-gradient(100deg, #FF31C9 0%, #5D78FF 52%, #2C98FF 100%)",
} as const;

export const colors = {
  bg: "#070A14",
  bgLift: "#0D1220",
  surface: "#111828",
  surfaceEdge: "#20293E",

  text: "#EAEFFA",
  textMuted: "#97A6C4",
  textDim: "#61708F",

  /** The engine: graph, backtest, determinism. */
  accent: brand.blue,
  accentSoft: "#7BC0FF",
  accentDeep: "#1A6FC4",

  /** Live: the wall clock, and the drift that motivates the whole film. */
  live: brand.magenta,
  liveSoft: "#FF74DC",

  indigo: brand.indigo,
  green: "#4ADE80",
} as const;

export const fonts = {
  /** Liberation Sans is metric-compatible with Helvetica and is always present. */
  display: '"Liberation Sans", "DejaVu Sans", sans-serif',
  mono: '"DejaVu Sans Mono", "Liberation Mono", monospace',
} as const;

/**
 * Caption band geometry. Sized for a phone held at arm's length in a feed --
 * these are the words a muted viewer actually reads, so they get the room, and
 * scenes 2-5 dropped their headings to pay for it.
 */
export const caption = {
  maxWidth: 900,
  fontSize: 38,
  bottom: 84,
} as const;

export const syntax = {
  plain: colors.text,
  keyword: "#C792EA",
  type: "#7BC0FF",
  method: "#8ED0FF",
  fn: "#82AAFF",
  string: "#C3E88D",
  number: "#F78C6C",
  macro: "#FFCB6B",
  punct: "#7E94B4",
  closure: "#FF74DC",
} as const;
