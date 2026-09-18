#!/usr/bin/env python3
"""Fail the build if the captions outrun a reader.

Subtitle practice measures reading load in **characters per second**. Netflix
caps English at 17 CPS and the BBC lands near the same place, but both assume a
viewer whose only job is reading. This film asks more: on scenes 2-5 the caption
competes with a code block or a live terminal for the same pair of eyes, so the
ceiling here is deliberately tighter.

A sentence's reading window runs from when it appears to when the next one
replaces it -- speech plus the silence that follows it -- which is exactly what
`build-voice.py` records, so this measures what a viewer actually gets.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Broadcast practice for a reader doing nothing else.
BROADCAST_CPS = 17.0
# Ours, for a reader also taking in code or a terminal.
LIMIT_CPS = 15.5


def main() -> None:
    narration = json.loads((ROOT / "assets/narration.json").read_text())
    rows, failures = [], []

    for scene in narration["scenes"]:
        for i, s in enumerate(scene["sentences"]):
            nxt = (
                scene["sentences"][i + 1]["start"]
                if i + 1 < len(scene["sentences"])
                else scene["duration"]
            )
            window = nxt - s["start"]
            cps = len(s["text"]) / window if window > 0 else float("inf")
            rows.append((scene["id"], scene["captions"], s["text"], window, cps))
            if scene["captions"] and cps > LIMIT_CPS:
                failures.append((scene["id"], s["text"], cps))

    width = max(len(r[2]) for r in rows)
    print(f"{'scene':<11} {'cap':<4} {'sentence':<{min(width, 62)}} {'secs':>5} {'CPS':>5}")
    print("-" * (11 + 5 + min(width, 62) + 13))
    for scene_id, captions, text, window, cps in rows:
        mark = ""
        if captions:
            mark = "  FAIL" if cps > LIMIT_CPS else ("  ~" if cps > BROADCAST_CPS * 0.9 else "")
        print(
            f"{scene_id:<11} {'yes' if captions else '—':<4} "
            f"{text[:62]:<{min(width, 62)}} {window:5.2f} {cps:5.1f}{mark}"
        )

    print(f"\ntotal {narration['total']:.2f}s")
    if failures:
        print(f"\n{len(failures)} captioned sentence(s) over {LIMIT_CPS} CPS:", file=sys.stderr)
        for scene_id, text, cps in failures:
            print(f"  {scene_id}: {cps:.1f} CPS — {text}", file=sys.stderr)
        print(
            "\nFix by slowing the voice (`voice.lengthScale` in scripts/script.json), "
            "widening `voice.sentenceSilence`, or shortening the line. Note that "
            "trimming words alone does NOT help: the window shrinks with the text, "
            "so CPS is set by the speaking rate, not the sentence length.",
            file=sys.stderr,
        )
        raise SystemExit(1)
    print(f"all captioned sentences within {LIMIT_CPS} CPS")


if __name__ == "__main__":
    main()
