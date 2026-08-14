/**
 * One palette and type scale for the whole film, so seven scenes read as one
 * piece. Dark ground, teal/cyan for the engine, amber reserved for time and
 * drift -- the two things the video is arguing about.
 */

export const colors = {
  bg: "#080D16",
  bgLift: "#0E1524",
  surface: "#121B2C",
  surfaceEdge: "#1E2B42",

  text: "#E8EFFA",
  textMuted: "#93A6C2",
  textDim: "#5E7191",

  accent: "#22D3EE",
  accentDeep: "#0E7490",
  accentSoft: "#67E8F9",

  amber: "#FBBF24",
  amberSoft: "#FCD34D",

  green: "#4ADE80",
  violet: "#A78BFA",
  rose: "#FB7185",
} as const;

export const fonts = {
  /** Liberation Sans is metric-compatible with Helvetica and is always present. */
  display: '"Liberation Sans", "DejaVu Sans", sans-serif',
  mono: '"DejaVu Sans Mono", "Liberation Mono", monospace',
} as const;

/** Caption band geometry -- fixed, because muted viewers read it every scene. */
export const caption = {
  maxWidth: 840,
  fontSize: 29,
  bottom: 84,
} as const;

export const syntax = {
  plain: colors.text,
  keyword: "#C792EA",
  type: "#7FDBCA",
  method: colors.accentSoft,
  fn: "#82AAFF",
  string: "#C3E88D",
  number: "#F78C6C",
  macro: "#FFCB6B",
  punct: "#7E94B4",
  closure: "#F07178",
} as const;
