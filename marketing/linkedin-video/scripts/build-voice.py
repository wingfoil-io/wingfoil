#!/usr/bin/env python3
"""Synthesise one wav per scene with piper, and derive the video's timing from it.

Each scene is synthesised a sentence at a time so the sentence boundaries are
*measured* rather than guessed, then stitched back together with a fixed silence
between sentences. Word timings are apportioned within each measured sentence,
which is what drives the karaoke captions and the sidecar SRT.

The output `assets/narration.json` is the single source of pacing: Remotion reads
it and gives every scene exactly as many frames as its wav needs. Swapping in a
human voiceover therefore means replacing the wavs and re-deriving this file --
no hand-tuned frame counts to chase.
"""

from __future__ import annotations

import argparse
import array
import json
import re
import subprocess
import sys
import tempfile
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Trailing pad after the voice stops, so a scene does not cut on the last phoneme.
TAIL_PAD = 0.45
# Lead-in before the voice starts, so a scene's visuals establish first.
LEAD_IN = 0.15


def split_sentences(text: str) -> list[str]:
    parts = re.split(r"(?<=[.!?])\s+", text.strip())
    return [p.strip() for p in parts if p.strip()]


def synth(piper: str, model: Path, cfg: dict, text: str, out: Path) -> None:
    cmd = [
        piper,
        "-m", str(model),
        "--length-scale", str(cfg["lengthScale"]),
        # Sentences are stitched by this script, so piper must not pad them itself.
        "--sentence-silence", "0",
        "-f", str(out),
    ]
    if cfg.get("speaker") is not None:
        cmd += ["-s", str(cfg["speaker"])]
    proc = subprocess.run(cmd, input=text, text=True, capture_output=True)
    if proc.returncode != 0 or not out.exists():
        raise SystemExit(f"piper failed on {text!r}:\n{proc.stderr}")


def read_wav(path: Path) -> tuple[bytes, int, int, int]:
    with wave.open(str(path), "rb") as w:
        return w.readframes(w.getnframes()), w.getframerate(), w.getnchannels(), w.getsampwidth()


def word_times(sentence: str, start: float, speech: float) -> list[dict]:
    """Apportion a measured sentence duration across its words.

    Weighted by word length -- a crude but stable model of speaking time, and
    accurate enough for a karaoke highlight because every *sentence* boundary is
    measured rather than estimated.
    """
    words = sentence.split()
    weights = [len(w) + 1 for w in words]
    total = sum(weights) or 1
    out, t = [], start
    for word, weight in zip(words, weights):
        dur = speech * weight / total
        out.append({"word": word, "start": round(t, 4), "end": round(t + dur, 4)})
        t += dur
    return out


def build_scene(scene: dict, piper: str, model: Path, cfg: dict, audio_dir: Path) -> dict:
    sentences = split_sentences(scene["text"])
    frames, rate, channels, width = b"", None, None, None
    sentence_spans, words = [], []
    cursor = LEAD_IN

    with tempfile.TemporaryDirectory() as tmp:
        for i, sentence in enumerate(sentences):
            part = Path(tmp) / f"{scene['id']}-{i}.wav"
            synth(piper, model, cfg, sentence, part)
            data, rate, channels, width = read_wav(part)
            speech = len(data) / (rate * channels * width)

            sentence_spans.append(
                {"text": sentence, "start": round(cursor, 4), "end": round(cursor + speech, 4)}
            )
            words += word_times(sentence, cursor, speech)
            frames += data
            cursor += speech

            if i != len(sentences) - 1:
                gap = int(rate * cfg["sentenceSilence"]) * channels * width
                frames += b"\0" * gap
                cursor += cfg["sentenceSilence"]

    lead = b"\0" * (int(rate * LEAD_IN) * channels * width)
    tail = b"\0" * (int(rate * TAIL_PAD) * channels * width)
    frames = lead + frames + tail

    out = audio_dir / f"{scene['id']}.wav"
    with wave.open(str(out), "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(width)
        w.setframerate(rate)
        w.writeframes(frames)

    if width != 2:
        raise SystemExit(f"expected 16-bit piper output, got {width * 8}-bit")
    samples = array.array("h")
    samples.frombytes(frames)
    peak = max(abs(s) for s in samples) / 32768.0
    if peak < 0.05:
        raise SystemExit(f"scene {scene['id']} synthesised near-silence (peak {peak:.3f})")

    audio_duration = len(frames) / (rate * channels * width)
    return {
        "id": scene["id"],
        "captions": scene["captions"],
        "text": scene["text"],
        "audio": f"audio/{scene['id']}.wav",
        # The wav's own length, plus any reading time the card asks for. Scene
        # length follows the voice; `hold` only ever extends it.
        "audioDuration": round(audio_duration, 4),
        "duration": round(audio_duration + scene.get("hold", 0.0), 4),
        "sentences": sentence_spans,
        "words": words,
    }


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--piper", default=str(ROOT / ".venv/bin/piper"))
    args = ap.parse_args()

    spec = json.loads((ROOT / "scripts/script.json").read_text())
    cfg = spec["voice"]
    model = ROOT / cfg["model"]
    if not model.exists():
        raise SystemExit(f"voice model missing: {model}\nRun scripts/fetch-voice.sh first.")

    # Remotion serves scene audio from public/, so write it straight there.
    audio_dir = ROOT / "public/audio"
    audio_dir.mkdir(parents=True, exist_ok=True)

    scenes = []
    for scene in spec["scenes"]:
        built = build_scene(scene, args.piper, model, cfg, audio_dir)
        scenes.append(built)
        print(f"  {built['id']:<11} {built['duration']:>6.2f}s", file=sys.stderr)

    total = sum(s["duration"] for s in scenes)
    out = ROOT / "assets/narration.json"
    out.write_text(json.dumps({"fps": 30, "total": round(total, 4), "scenes": scenes}, indent=2))
    print(f"\ntotal {total:.2f}s -> {out}", file=sys.stderr)


if __name__ == "__main__":
    main()
