# wingfoil LinkedIn video

A 1080×1080, ~36s marketing film for LinkedIn's muted mobile feed, rendered from
code. One message, and only one:

> **One graph definition runs as both an instant historical backtest and a live
> realtime system, so backtest and production can't drift.**

Everything on screen is either code in this repo or output captured from running
it. The pipeline is fully offline and costs nothing to re-run.

## Deliverables

| File | What it is |
| --- | --- |
| `out/wingfoil_linkedin.mp4` | The video — 1080×1080, 30fps, H.264 |
| `out/tutorial_li.srt` | Sidecar subtitles, same timings as the burned-in captions |

Captions are **burned in** on the scenes that need them as well as shipped as a
sidecar, because most of this audience watches muted and LinkedIn's own
auto-captions are not reliable enough to depend on.

## Build it

```sh
cd marketing/linkedin-video

# once: toolchain + the LibriTTS-R voice (~80MB, not committed)
python3 -m venv .venv && .venv/bin/pip install piper-tts
scripts/fetch-voice.sh
npm install

npm run build          # capture → voice → srt → render
```

Or a step at a time:

```sh
scripts/capture-output.sh     # run the example, record real output
python3 scripts/build-voice.py  # synthesise narration, measure it
python3 scripts/build-srt.py    # sidecar subtitles
npm run render                  # Remotion → mp4
npm start                       # Remotion studio, for iterating on scenes
```

## How it fits together

```
scripts/script.json ──► build-voice.py ──► public/audio/*.wav
                                       └─► assets/narration.json ──┐
                                                                   ├─► src/  ──► mp4
crates/…/odds_evens ──► capture-output.sh ──► assets/terminal.json ─┘
                                       └─► build-srt.py ──► out/tutorial_li.srt
```

Two rules hold the whole thing together:

**Pacing follows the voice.** `build-voice.py` synthesises one wav per scene and
measures it; `assets/narration.json` carries those durations, and
`src/narration.ts` turns them into frame counts. No scene length is typed by
hand anywhere. That is the seam for swapping in a human voiceover — see below.

**Terminal output is captured, never written.** `scripts/capture-output.sh` runs
the committed `crates/wingfoil/examples/core/odds_evens` example once per run
mode and records what it printed, along with the command that printed it. Only
`env_logger`'s volatile wall-clock prefix is stripped; the `<engine-time> <label>
"<value>"` payload is exactly what `logged` emitted. The capture fails loudly if
the two run modes ever stop producing identical values, because that is the
claim the video makes.

## Brand

The mark in `public/brand/wingfoil-mark.png` is the real logo, and the palette
in `src/theme.ts` is sampled from its gradient:

| Token | Hex | Means |
| --- | --- | --- |
| `brand.magenta` | `#FF31C9` | **Live** — the wall clock, and the drift |
| `brand.blue` | `#2C98FF` | **The engine** — the graph, the replay, determinism |
| `brand.indigo` | `#5D78FF` | Where they meet (`merge`, the cycle clock) |

That split is not decoration; it is the argument. The hook's two rules are
magenta and blue drifting apart, and the payoff converges them into a single
rule painted in `brand.gradient` — the same magenta-to-blue sweep the logo is
drawn in. Scene 3 gives one branch each colour and makes `merge` the indigo
between them; scenes 4 and 5 colour the engine-time column blue for the replay
and magenta for the live run.

> **Source, and its caveat.** These came from the Sept 2025 talk deck in
> `wingfoil-io/assets`, because **wingfoil.io is blocked by the sandbox's egress
> proxy** and could not be read. If the site has since moved on, re-sample the
> three hexes in `src/theme.ts` and drop in a new `wingfoil-mark.png` — every
> scene reads from those and nothing else hardcodes a brand colour.

## Scenes

| # | Scene | Captions | Carries |
| --- | --- | --- | --- |
| 1 | Hook | — | Two codebases, drifting apart |
| 2 | Code | ✓ | The snippet, revealed line by line |
| 3 | DAG | ✓ | The diamond, one clock notch per full cycle |
| 4 | Historical | ✓ | Captured replay output, effectively instant |
| 5 | Realtime | ✓ | Captured live output, paced by the wall clock |
| 6 | Payoff | — | Same graph, same results |
| 7 | CTA | — | The mark, the wordmark, link in the comments |

Scenes 1, 6 and 7 carry their own large type instead of a caption band — the
words *are* the visual, and a band under them would be saying it twice.

Scenes 2–5 carry **nothing but the visual and one caption band** — no headings,
no badge pills. Both were saying, in a third voice, what the narration and the
caption were already saying, and stripping them bought the caption a jump from
29px to **38px**, which is the size that matters for a muted viewer scrolling a
phone.

What tells scenes 4 and 5 apart is now the evidence itself rather than a label
on it: the command line (`-- realtime`), the engine-time column (blue and
zero-based for the replay, magenta and epoch for the live run), and the
`waiting…` prompt that only the live run has.

### What scenes 4 and 5 actually show

This is the one place the film's design was decided by the engine rather than by
taste, so it is worth stating plainly.

`RunMode::HistoricalFrom(NanoTime::ZERO)` makes engine time pure logic, so the
replay stamps `0.000_000 … 0.050_000`. `RunMode::RealTime` makes engine time the
**wall clock**, so the same six ticks stamp as epoch nanoseconds
(`1,786,734,263.432_339 …`). The timestamps are *not* identical across modes, and
the video does not claim they are.

What is identical — byte for byte, and asserted by the capture script — is the
**values and their order**. So scene 5 renders the real live capture with the
clock column dimmed to the brand's magenta and the value column held bright, and
the narration says only what is true: same values, same order, only the clock
changes. The eye compares the bright halves across the two scenes and finds them
the same; the dim halves differ, which is exactly what a wall clock is supposed
to do.

## Swapping in a human voiceover

Scene durations are derived, so re-recording the narration is a drop-in:

1. Record one file per scene, named `hook.wav`, `code.wav`, … into `public/audio/`.
2. Re-derive the timings. `build-voice.py` currently *generates* the wavs; for
   human audio, replace the synthesis step with
   [`@remotion/install-whisper-cpp`](https://remotion.dev/docs/install-whisper-cpp)
   to transcribe each file, which gives **word-level** timings and therefore
   tighter karaoke than the current apportioning.
3. `npm run render`.

Nothing in `src/` needs touching — every frame count reads from
`assets/narration.json`.

Until then, word timings within a sentence are apportioned by word length. Every
*sentence* boundary is measured from its own wav, so the drift within a sentence
never accumulates.

## Rendering notes

Remotion downloads its own headless shell on first render. Do **not** point it at
the container's `/opt/pw-browsers/chromium` — full Chromium rejects the
old-headless mode Remotion uses and the launch fails.

Type is set in Liberation Sans and DejaVu Sans Mono, both present on the render
host, so no font is fetched at build time and the render stays offline.

## Posting

`copy.md` holds the post text, the first comment, and the upload checklist.
Upload the MP4 **natively** — a native video gets far more reach than a link out.
