#!/usr/bin/env python3
"""Write the sidecar subtitle track from the same timings the burned-in captions use.

`assets/narration.json` carries per-scene sentence spans measured from the wavs;
this walks the scenes in order, offsets each span by that scene's start, and
emits SRT. The sidecar and the on-screen captions therefore cannot drift -- they
are two renderings of one source.
"""

from __future__ import annotations

import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def stamp(seconds: float) -> str:
    ms = int(round(seconds * 1000))
    h, ms = divmod(ms, 3_600_000)
    m, ms = divmod(ms, 60_000)
    s, ms = divmod(ms, 1000)
    return f"{h:02d}:{m:02d}:{s:02d},{ms:03d}"


def main() -> None:
    narration = json.loads((ROOT / "assets/narration.json").read_text())
    fps = narration["fps"]

    cues: list[tuple[float, float, str]] = []
    offset = 0.0
    for scene in narration["scenes"]:
        frames = max(1, round(scene["duration"] * fps))
        for sentence in scene["sentences"]:
            cues.append((offset + sentence["start"], offset + sentence["end"], sentence["text"]))
        offset += frames / fps

    # Hold each cue until the next begins, within its own scene, so there is no
    # flicker of empty subtitle between sentences.
    lines = []
    for i, (start, end, text) in enumerate(cues):
        nxt = cues[i + 1][0] if i + 1 < len(cues) else end + 0.6
        lines += [str(i + 1), f"{stamp(start)} --> {stamp(min(nxt - 0.05, end + 0.6))}", text, ""]

    out = ROOT / "out/tutorial_li.srt"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(lines))
    print(f"wrote {out}: {len(cues)} cues over {offset:.2f}s")


if __name__ == "__main__":
    main()
