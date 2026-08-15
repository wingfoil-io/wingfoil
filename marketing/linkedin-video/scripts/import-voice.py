#!/usr/bin/env python3
"""Re-time the film around a human voiceover.

`build-voice.py` synthesises the narration and measures it. This does the same
job for recordings you made yourself: it takes one wav per scene, measures each,
and rewrites `assets/narration.json` so every scene gets exactly as many frames
as your audio needs. Nothing in `src/` changes -- frame counts have always been
derived, never typed.

    scripts/import-voice.py --from ~/vo        # wavs named hook.wav, code.wav, ...
    python3 scripts/build-srt.py               # sidecar subtitles
    python3 scripts/check-pacing.py            # can a reader keep up?
    npm run render

**Word timings.** Piper is synthesised a sentence at a time, so its sentence
boundaries are *measured*. A single human wav cannot give that up, so by default
this apportions the scene's known script text across the measured duration by
character weight -- good enough for a caption band, and the pacing gate still
applies. For real word-level karaoke, drop a `<scene>.words.json` beside the wav:

    [{"word": "Run", "start": 0.15, "end": 0.34}, ...]

which is the shape `@remotion/install-whisper-cpp` produces. Any scene that has
one uses it; the rest fall back to apportioning.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import re
import sys
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FPS = 30


def split_sentences(text: str) -> list[str]:
    parts = re.split(r"(?<=[.!?])\s+", text.strip())
    return [p.strip() for p in parts if p.strip()]


def wav_duration(path: Path) -> float:
    with contextlib.closing(wave.open(str(path), "rb")) as w:
        return w.getnframes() / float(w.getframerate())


def apportion(text: str, start: float, span: float):
    """Spread a scene's sentences and words across a measured duration.

    Weighted by character count -- crude, but the alternative is inventing
    boundaries, and the pacing gate checks the result either way.
    """
    sentences = split_sentences(text)
    weights = [len(s) for s in sentences]
    total = sum(weights) or 1

    spans, words, cursor = [], [], start
    for sentence, weight in zip(sentences, weights):
        dur = span * weight / total
        spans.append({"text": sentence, "start": round(cursor, 4), "end": round(cursor + dur, 4)})

        ws = sentence.split()
        wweights = [len(w) + 1 for w in ws]
        wtotal = sum(wweights) or 1
        t = cursor
        for word, ww in zip(ws, wweights):
            wd = dur * ww / wtotal
            words.append({"word": word, "start": round(t, 4), "end": round(t + wd, 4)})
            t += wd
        cursor += dur
    return spans, words


def from_whisper(text: str, entries: list[dict]):
    """Use real word timings, grouping them back into the script's sentences."""
    spans, words, i = [], [], 0
    for sentence in split_sentences(text):
        n = len(sentence.split())
        chunk = entries[i : i + n]
        if len(chunk) != n:
            raise SystemExit(
                f"word-timing file has {len(entries)} words but the script needs more; "
                "does the recording match scripts/script.json?"
            )
        spans.append(
            {"text": sentence, "start": round(chunk[0]["start"], 4), "end": round(chunk[-1]["end"], 4)}
        )
        words += [
            {"word": w, "start": round(c["start"], 4), "end": round(c["end"], 4)}
            for w, c in zip(sentence.split(), chunk)
        ]
        i += n
    return spans, words


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--from", dest="src", required=True, help="directory of <scene>.wav recordings")
    ap.add_argument(
        "--hold",
        action="store_true",
        help="keep the `hold` values from script.json (extra on-screen time after the voice stops)",
    )
    args = ap.parse_args()

    src = Path(args.src).expanduser()
    spec = json.loads((ROOT / "scripts/script.json").read_text())
    audio_dir = ROOT / "public/audio"
    audio_dir.mkdir(parents=True, exist_ok=True)

    scenes = []
    for scene in spec["scenes"]:
        wav = src / f"{scene['id']}.wav"
        if not wav.exists():
            raise SystemExit(
                f"missing {wav}\nExpected one wav per scene, named: "
                + ", ".join(f"{s['id']}.wav" for s in spec["scenes"])
            )
        duration = wav_duration(wav)

        timings = src / f"{scene['id']}.words.json"
        if timings.exists():
            spans, words = from_whisper(scene["text"], json.loads(timings.read_text()))
            source = "whisper"
        else:
            spans, words = apportion(scene["text"], 0.0, duration)
            source = "apportioned"

        # Copy rather than reference, so public/ stays the single place Remotion
        # serves audio from, exactly as the synthesised path leaves it.
        (audio_dir / f"{scene['id']}.wav").write_bytes(wav.read_bytes())

        hold = scene.get("hold", 0.0) if args.hold else 0.0
        scenes.append(
            {
                "id": scene["id"],
                "captions": scene["captions"],
                "text": scene["text"],
                "audio": f"audio/{scene['id']}.wav",
                "audioDuration": round(duration, 4),
                "duration": round(duration + hold, 4),
                "sentences": spans,
                "words": words,
            }
        )
        print(f"  {scene['id']:<11} {duration:>6.2f}s  ({source})", file=sys.stderr)

    total = sum(s["duration"] for s in scenes)
    out = ROOT / "assets/narration.json"
    out.write_text(json.dumps({"fps": FPS, "total": round(total, 4), "scenes": scenes}, indent=2))
    print(f"\ntotal {total:.2f}s -> {out}", file=sys.stderr)
    print(
        "next: python3 scripts/check-pacing.py && python3 scripts/build-srt.py && npm run render",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
