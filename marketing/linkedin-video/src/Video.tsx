import React from "react";
import { AbsoluteFill, Audio, Sequence, staticFile } from "remotion";

import { Captions } from "./components/Captions";
import { Stage } from "./components/Stage";
import { layout, type Scene } from "./narration";
import { Code } from "./scenes/Code";
import { Cta } from "./scenes/Cta";
import { Dag } from "./scenes/Dag";
import { Historical } from "./scenes/Historical";
import { Hook } from "./scenes/Hook";
import { Payoff } from "./scenes/Payoff";
import { Realtime } from "./scenes/Realtime";
import { colors } from "./theme";

const SCENES: Record<string, React.FC<{ durationInFrames: number }>> = {
  hook: () => <Hook />,
  code: ({ durationInFrames }) => <Code durationInFrames={durationInFrames} />,
  dag: ({ durationInFrames }) => <Dag durationInFrames={durationInFrames} />,
  historical: () => <Historical />,
  realtime: ({ durationInFrames }) => <Realtime durationInFrames={durationInFrames} />,
  payoff: () => <Payoff />,
  cta: () => <Cta />,
};

/**
 * One scene: its own audio, its own visuals, and -- where the scene carries
 * captions -- the karaoke band on top of both.
 *
 * The frame count comes from the scene's wav, never from a hand-picked number,
 * so re-recording the voice re-times the film.
 */
const SceneSlot: React.FC<{ scene: Scene; durationInFrames: number; index: number }> = ({
  scene,
  durationInFrames,
  index,
}) => {
  const Body = SCENES[scene.id];
  if (!Body) {
    throw new Error(`narration.json has scene "${scene.id}" with no component`);
  }

  return (
    <Stage durationInFrames={durationInFrames} seed={index}>
      <Audio src={staticFile(scene.audio)} />
      <Body durationInFrames={durationInFrames} />
      {scene.captions ? <Captions words={scene.words} sentences={scene.sentences} /> : null}
    </Stage>
  );
};

export const WingfoilLinkedIn: React.FC = () => (
  <AbsoluteFill style={{ background: colors.bg }}>
    {layout().map(({ scene, from, durationInFrames }, i) => (
      <Sequence key={scene.id} from={from} durationInFrames={durationInFrames} name={scene.id}>
        <SceneSlot scene={scene} durationInFrames={durationInFrames} index={i} />
      </Sequence>
    ))}
  </AbsoluteFill>
);
