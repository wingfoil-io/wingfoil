#!/usr/bin/env python3
"""Split one continuous voiceover take into the per-scene wavs the film wants.

Recording seven separate files is a chore. Record one take instead, leaving a
clear pause between lines, and this cuts it on those pauses:

    scripts/split-voice.py --from take.wav --to ~/vo     # check the cuts
    scripts/import-voice.py --from ~/vo                  # re-time the film

Segments come out in `scripts/script.json` order, so **read the lines in order**
and leave roughly a second of silence between them — longer than any pause you
take mid-sentence, which is what keeps the cut points unambiguous.

It refuses to write anything unless it finds exactly as many segments as there
are scenes, and prints each one beside the line it will become, so a
mis-detection is visible before it reaches the render. No dependencies: pure
`wave` plus RMS, same as the rest of the pipeline.

**When no threshold works, say where the cuts go.** A take that ran two lines
together with a shorter pause than one taken mid-sentence cannot be separated by
`--gap` at all — the first real take had a 0.66s line break and a 0.76s pause
inside the next line, so every threshold gets one of them wrong. `--at` takes
the boundary times directly:

    scripts/split-voice.py --from take.wav --at 5.9,13.3,19.3,23.2,29.3,33.7

One fewer time than there are scenes, each landing anywhere in the silence
between two lines; speech is grouped around them, so the exact value inside a
pause does not matter. Read the times off a dry run, or off any waveform editor.
"""

from __future__ import annotations

import argparse
import array
import contextlib
import json
import math
import wave
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# Analysis window. Short enough to place a cut precisely, long enough that one
# glottal click does not read as speech.
FRAME_MS = 20


def read_wav(path: Path):
    with contextlib.closing(wave.open(str(path), "rb")) as w:
        params = w.getparams()
        return w.readframes(w.getnframes()), params


def mono_amplitudes(frames: bytes, params) -> list[float]:
    """Per-sample amplitude in [0, 1], averaged across channels."""
    width, channels = params.sampwidth, params.nchannels
    if width == 2:
        samples = array.array("h")
        samples.frombytes(frames)
        scale = 32768.0
    elif width == 4:
        samples = array.array("i")
        samples.frombytes(frames)
        scale = 2147483648.0
    else:
        raise SystemExit(
            f"{width * 8}-bit wav is not supported — convert to 16-bit first:\n"
            "  ffmpeg -i take.wav -acodec pcm_s16le -ar 48000 take16.wav"
        )
    if channels == 1:
        return [abs(s) / scale for s in samples]
    return [
        sum(abs(samples[i + c]) for c in range(channels)) / (channels * scale)
        for i in range(0, len(samples) - channels + 1, channels)
    ]


def find_segments(amps: list[float], rate: int, floor_db: float, gap_s: float):
    """Runs of speech, separated by at least `gap_s` of near-silence."""
    frame = max(1, int(rate * FRAME_MS / 1000))
    floor = 10 ** (floor_db / 20)

    loud = []
    for start in range(0, len(amps), frame):
        window = amps[start : start + frame]
        rms = math.sqrt(sum(a * a for a in window) / len(window)) if window else 0.0
        loud.append(rms >= floor)

    gap_frames = max(1, int(gap_s * 1000 / FRAME_MS))
    segments, run_start, silence = [], None, 0
    for i, is_loud in enumerate(loud):
        if is_loud:
            if run_start is None:
                run_start = i
            silence = 0
        elif run_start is not None:
            silence += 1
            if silence >= gap_frames:
                segments.append((run_start, i - silence + 1))
                run_start, silence = None, 0
    if run_start is not None:
        segments.append((run_start, len(loud)))
    return [(a * frame, b * frame) for a, b in segments]


def segments_at(amps: list[float], rate: int, floor_db: float, cuts: list[float]):
    """Speech grouped around explicit boundary times.

    Blocks are detected with a deliberately short gap so that every pause is a
    block boundary, then grouped by how many cut times precede them. A cut
    landing anywhere inside a silence therefore lands on the same boundary,
    which is why the caller only has to be roughly right.
    """
    blocks = find_segments(amps, rate, floor_db, 0.25)
    if not blocks:
        raise SystemExit("no speech found — check --floor")

    groups: list[list[tuple[int, int]]] = [[] for _ in range(len(cuts) + 1)]
    for a, b in blocks:
        i = sum(1 for c in cuts if a >= c * rate)
        groups[i].append((a, b))

    empty = [i for i, g in enumerate(groups) if not g]
    if empty:
        raise SystemExit(
            f"no speech between cuts for segment(s) {', '.join(str(i + 1) for i in empty)} "
            "— two --at times fell inside the same silence, or one is past the end"
        )
    return [(g[0][0], g[-1][1]) for g in groups]


def main() -> None:
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    ap.add_argument("--from", dest="src", required=True, help="the single take (wav)")
    ap.add_argument("--to", dest="dst", help="directory to write per-scene wavs into")
    ap.add_argument(
        "--gap",
        type=float,
        default=0.9,
        help="minimum silence between lines, seconds (default 0.9)",
    )
    ap.add_argument(
        "--floor",
        type=float,
        default=-40.0,
        help="silence threshold in dBFS (default -40; raise toward -30 for a noisy room)",
    )
    ap.add_argument(
        "--at",
        help="explicit boundary times in seconds, comma separated, one fewer than "
        "there are scenes — overrides --gap",
    )
    ap.add_argument(
        "--pad",
        type=float,
        default=0.12,
        help="silence kept either side of each segment, seconds (default 0.12)",
    )
    args = ap.parse_args()

    spec = json.loads((ROOT / "scripts/script.json").read_text())
    scenes = spec["scenes"]

    src = Path(args.src).expanduser()
    if not src.is_file():
        raise SystemExit(f"no such file: {src}")
    frames, params = read_wav(src)
    amps = mono_amplitudes(frames, params)
    rate, channels, width = params.framerate, params.nchannels, params.sampwidth

    if args.at:
        cuts = [float(t) for t in args.at.split(",") if t.strip()]
        if len(cuts) != len(scenes) - 1:
            raise SystemExit(
                f"--at needs {len(scenes) - 1} times for {len(scenes)} scenes, got {len(cuts)}"
            )
        segments = segments_at(amps, rate, args.floor, sorted(cuts))
    else:
        segments = find_segments(amps, rate, args.floor, args.gap)
    total = len(amps) / rate
    print(f"{src.name}: {total:.1f}s, found {len(segments)} segment(s)\n")

    pad = int(args.pad * rate)
    rows = []
    for i, (a, b) in enumerate(segments):
        a, b = max(0, a - pad), min(len(amps), b + pad)
        scene = scenes[i]["id"] if i < len(scenes) else "—"
        line = scenes[i]["text"] if i < len(scenes) else "(no scene for this segment)"
        rows.append((scene, a, b))
        print(f"  {i + 1}. {a / rate:6.2f}s → {b / rate:6.2f}s  ({(b - a) / rate:5.2f}s)  {scene}")
        print(f"      {line[:72]}{'…' if len(line) > 72 else ''}")

    if len(segments) != len(scenes):
        raise SystemExit(
            f"\nexpected {len(scenes)} segments, found {len(segments)}.\n"
            "Too few: the pauses between lines are shorter than --gap, or one line "
            "has a mid-sentence pause longer than it.\n"
            "Too many: a pause *inside* a line was long enough to cut on.\n"
            f"Try --gap (now {args.gap}s) or --floor (now {args.floor} dBFS), and "
            "re-run without --to until the segments line up."
        )

    if not args.dst:
        print("\nlooks right? re-run with --to <dir> to write the files")
        return

    dst = Path(args.dst).expanduser()
    dst.mkdir(parents=True, exist_ok=True)
    for scene, a, b in rows:
        out = dst / f"{scene}.wav"
        with contextlib.closing(wave.open(str(out), "wb")) as w:
            w.setnchannels(channels)
            w.setsampwidth(width)
            w.setframerate(rate)
            w.writeframes(frames[a * channels * width : b * channels * width])
    print(f"\nwrote {len(rows)} files to {dst}")
    print(f"next: scripts/import-voice.py --from {dst}")


if __name__ == "__main__":
    main()
