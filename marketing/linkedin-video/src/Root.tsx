import React from "react";
import { Composition } from "remotion";

import { FPS, totalFrames } from "./narration";
import { WingfoilLinkedIn } from "./Video";

/**
 * One composition, 1080x1080 -- the square that fills the most of a LinkedIn
 * mobile feed. Its length is the sum of the scene wavs, so nothing here needs
 * touching when the narration changes.
 */
export const RemotionRoot: React.FC = () => (
  <Composition
    id="WingfoilLinkedIn"
    component={WingfoilLinkedIn}
    durationInFrames={totalFrames()}
    fps={FPS}
    width={1080}
    height={1080}
  />
);
