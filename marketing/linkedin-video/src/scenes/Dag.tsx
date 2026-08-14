import React from "react";
import { AbsoluteFill, interpolate, useCurrentFrame, useVideoConfig } from "remotion";

import { colors, fonts } from "../theme";

type P = { x: number; y: number };

/**
 * The diamond, coloured by the brand's argument: the two branches take the two
 * brand colours, and `merge` is the indigo they meet in -- the same sweep the
 * logo is drawn in.
 */
const NODES: Record<string, P & { w: number; label: string; tone: string }> = {
  count: { x: 350, y: 52, w: 138, label: "count", tone: colors.indigo },
  evens: { x: 132, y: 214, w: 138, label: "evens", tone: colors.accent },
  odds: { x: 568, y: 214, w: 128, label: "odds", tone: colors.live },
  merge: { x: 350, y: 376, w: 138, label: "merge", tone: colors.indigo },
  log: { x: 350, y: 512, w: 128, label: "log", tone: colors.green },
};

const NODE_H = 58;

/** Edge endpoints, trimmed to the node boxes so the pulse starts at the border. */
const EDGES: [keyof typeof NODES, keyof typeof NODES][] = [
  ["count", "evens"],
  ["count", "odds"],
  ["evens", "merge"],
  ["odds", "merge"],
  ["merge", "log"],
];

const anchor = (key: keyof typeof NODES, side: "top" | "bottom"): P => {
  const n = NODES[key];
  return { x: n.x, y: side === "top" ? n.y - NODE_H / 2 : n.y + NODE_H / 2 };
};

const edgePoints = (from: keyof typeof NODES, to: keyof typeof NODES): [P, P] => [
  anchor(from, "bottom"),
  anchor(to, "top"),
];

const lerp = (a: P, b: P, t: number): P => ({ x: a.x + (b.x - a.x) * t, y: a.y + (b.y - a.y) * t });

/**
 * Scene 3 -- the graph the snippet built, with a pulse sweeping one traversal
 * at a time.
 *
 * The clock is the point of the scene: it advances exactly one 30-degree notch
 * per *completed* traversal, not on a free-running timer. That is what "the
 * engine runs each node once per cycle" looks like -- both branches carry the
 * pulse at once and `merge` fires once, so recombining does not cost an extra
 * tick.
 */
export const Dag: React.FC<{ durationInFrames: number }> = ({ durationInFrames }) => {
  const frame = useCurrentFrame();
  const { fps } = useVideoConfig();

  const startAt = Math.round(0.4 * fps);
  const cycleFrames = Math.round(1.15 * fps);
  const elapsed = Math.max(0, frame - startAt);
  const cycle = Math.floor(elapsed / cycleFrames);
  const phase = (elapsed % cycleFrames) / cycleFrames;
  const running = frame >= startAt;

  // Three legs of one traversal: fan out, recombine, emit.
  const legOf = (p: number): { leg: 0 | 1 | 2; t: number } =>
    p < 0.36
      ? { leg: 0, t: p / 0.36 }
      : p < 0.72
        ? { leg: 1, t: (p - 0.36) / 0.36 }
        : { leg: 2, t: (p - 0.72) / 0.28 };

  const { leg, t } = legOf(phase);

  /** Each pulse carries the colour of the branch it is travelling. */
  const pulses: (P & { tone: string })[] = !running
    ? []
    : leg === 0
      ? [
          { ...lerp(...edgePoints("count", "evens"), t), tone: colors.accentSoft },
          { ...lerp(...edgePoints("count", "odds"), t), tone: colors.liveSoft },
        ]
      : leg === 1
        ? [
            { ...lerp(...edgePoints("evens", "merge"), t), tone: colors.accentSoft },
            { ...lerp(...edgePoints("odds", "merge"), t), tone: colors.liveSoft },
          ]
        : [{ ...lerp(...edgePoints("merge", "log"), t), tone: colors.indigo }];

  /** How lit a node is right now: 1 while the pulse is departing it. */
  const heat = (key: keyof typeof NODES): number => {
    if (!running) return 0;
    const decay = (x: number) => Math.max(0, 1 - x * 3.2);
    switch (key) {
      case "count":
        return leg === 0 ? decay(t) : 0;
      case "evens":
      case "odds":
        return leg === 1 ? decay(t) : 0;
      case "merge":
        return leg === 2 ? decay(t) : 0;
      case "log":
        return leg === 2 ? Math.max(0, 1 - Math.abs(t - 1) * 4) : 0;
      default:
        return 0;
    }
  };

  // The clock: one notch per completed cycle, snapped rather than swept.
  const completed = running ? cycle : 0;
  const sinceTick = elapsed % cycleFrames;
  const snap = interpolate(sinceTick, [0, 5], [0, 1], {
    extrapolateLeft: "clamp",
    extrapolateRight: "clamp",
  });
  const handAngle = (completed - 1 + snap) * 30;
  const flash = running && completed > 0 ? Math.max(0, 1 - sinceTick / 7) : 0;

  return (
    <AbsoluteFill>
      <AbsoluteFill style={{ justifyContent: "center", alignItems: "center", paddingBottom: 198 }}>
        <svg width={820} height={648} viewBox="0 0 700 580">
          <defs>
            <filter id="glow" x="-60%" y="-60%" width="220%" height="220%">
              <feGaussianBlur stdDeviation="6" result="b" />
              <feMerge>
                <feMergeNode in="b" />
                <feMergeNode in="SourceGraphic" />
              </feMerge>
            </filter>
            <marker id="head" markerWidth="7" markerHeight="7" refX="5.4" refY="3" orient="auto">
              <path d="M0,0 L6,3 L0,6 Z" fill="#3D4F72" />
            </marker>
          </defs>

          {EDGES.map(([from, to], i) => {
            const [a, b] = edgePoints(from, to);
            return (
              <line
                key={i}
                x1={a.x}
                y1={a.y}
                x2={b.x}
                y2={b.y}
                stroke="#3D4F72"
                strokeWidth={2.6}
                markerEnd="url(#head)"
              />
            );
          })}

          {Object.entries(NODES).map(([key, n]) => {
            const h = heat(key as keyof typeof NODES);
            return (
              <g key={key}>
                <rect
                  x={n.x - n.w / 2}
                  y={n.y - NODE_H / 2}
                  width={n.w}
                  height={NODE_H}
                  rx={14}
                  fill={h > 0.02 ? `${n.tone}26` : colors.surface}
                  stroke={h > 0.02 ? n.tone : "#374766"}
                  strokeWidth={2 + h * 1.6}
                />
                <text
                  x={n.x}
                  y={n.y + 9}
                  textAnchor="middle"
                  fontFamily={fonts.mono}
                  fontSize={25}
                  fontWeight={700}
                  fill={h > 0.02 ? n.tone : "#B2C1DB"}
                >
                  {n.label}
                </text>
              </g>
            );
          })}

          {pulses.map((p, i) => (
            <circle key={i} cx={p.x} cy={p.y} r={9} fill={p.tone} filter="url(#glow)" />
          ))}
        </svg>
      </AbsoluteFill>

      {/* The clock -- one notch per completed traversal. */}
      <div style={{ position: "absolute", top: 96, right: 66 }}>
        <svg width={132} height={132} viewBox="0 0 132 132">
          <circle
            cx={66}
            cy={66}
            r={54}
            fill="rgba(9, 14, 26, 0.85)"
            stroke={colors.indigo}
            strokeWidth={2 + flash * 3}
            opacity={0.6 + flash * 0.4}
          />
          {Array.from({ length: 12 }).map((_, i) => (
            <line
              key={i}
              x1={66}
              y1={20}
              x2={66}
              y2={i % 3 === 0 ? 31 : 27}
              stroke={colors.indigo}
              strokeWidth={i % 3 === 0 ? 3 : 1.6}
              opacity={0.55}
              transform={`rotate(${i * 30} 66 66)`}
            />
          ))}
          <line
            x1={66}
            y1={66}
            x2={66}
            y2={30}
            stroke={colors.accentSoft}
            strokeWidth={4}
            strokeLinecap="round"
            transform={`rotate(${handAngle} 66 66)`}
          />
          <circle cx={66} cy={66} r={5} fill={colors.accentSoft} />
        </svg>
        <div
          style={{
            marginTop: 8,
            textAlign: "center",
            fontFamily: fonts.mono,
            fontSize: 19,
            color: colors.indigo,
            opacity: 0.9,
          }}
        >
          1 tick / cycle
        </div>
      </div>
    </AbsoluteFill>
  );
};
