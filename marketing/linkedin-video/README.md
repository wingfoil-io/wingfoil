# wingfoil LinkedIn video

A 1080×1080, ~46s marketing film for LinkedIn's muted mobile feed, rendered from
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
scripts/capture-output.sh        # run the example, record real output
python3 scripts/build-voice.py   # synthesise narration, measure it
python3 scripts/check-pacing.py  # gate: can a reader keep up?
python3 scripts/build-srt.py     # sidecar subtitles
npm run render                   # Remotion → mp4
npm start                        # Remotion studio, for iterating on scenes
```

## Pacing, and why it is a gate

Muted viewers *read* this film, so reading speed is a correctness property, not
a matter of taste. `scripts/check-pacing.py` measures every captioned sentence
in **characters per second** over its real on-screen window — from when it
appears to when the next one replaces it — and fails the build over **15.5
CPS**. Netflix caps English subtitles at 17 and the BBC lands near the same
place, but both assume a viewer whose only job is reading; here the caption
competes with a code block or a live terminal, so the ceiling is tighter.

The first cut ran **18–20 CPS on six of eight** captioned sentences. It now
peaks at 14.5.

The counter-intuitive part, and the reason the failure message spells it out:
**shortening a sentence does not lower its CPS.** The window shrinks with the
text, so the ratio barely moves. CPS is set by the *speaking rate*, and the
only real levers are `voice.lengthScale`, `voice.sentenceSilence`, and a scene
`hold` for the sentence that ends a scene. Trimming words is still worth doing
— it buys back *length* budget so the voice can slow down without the film
growing.

### Reproducibility

`voice.noiseWScale` is pinned to `0`. VITS predicts phoneme durations
stochastically, so the same text synthesised twice differed by ~2% — enough to
re-time every scene, shift every caption, and make the pacing gate flaky. With
it pinned, `assets/narration.json` is byte-identical across runs, so a re-render
reproduces the same film.

## How it fits together

```
scripts/script.json ──► build-voice.py ──► public/audio/*.wav
                                       └─► assets/narration.json ──┐
                                                                   ├─► src/  ──► mp4
crates/…/top_of_book ──► capture-output.sh ──► assets/terminal.json ─┘
                                       └─► build-srt.py ──► out/tutorial_li.srt
```

Two rules hold the whole thing together:

**Pacing follows the voice.** `build-voice.py` synthesises one wav per scene and
measures it; `assets/narration.json` carries those durations, and
`src/narration.ts` turns them into frame counts. No scene length is typed by
hand anywhere. That is the seam for swapping in a human voiceover — see below.

**Terminal output is captured, never written.** `scripts/capture-output.sh` runs
the committed `crates/wingfoil/examples/core/top_of_book` example once per run
mode -- real NASDAQ AAPL messages from the LOBSTER sample, through a real limit
order book -- and records what it printed, along with the command that printed
it. Nothing is reformatted. The capture fails loudly if the two run modes ever
stop producing identical quotes, because that is the claim the video makes.

The example puts the run-mode branch behind a **`MarketData` trait**: `Replay`
stamps each message with its own time and lets the engine schedule it,
`LiveFeed` hands them over at the pace they arrived, and `market_data(run_mode)`
picks one. Scene 2 opens on that line — `let feed = market_data(run_mode)?.connect(&g)?;`
under the comment *"the only line that differs between backtest and live"* —
because it is the film's argument in one statement.

## Brand

The mark in `public/brand/wingfoil-mark.png` is the real logo, and the palette
in `src/theme.ts` is sampled from its gradient:

| Token | Hex | Means |
| --- | --- | --- |
| `brand.magenta` | `#FF31C9` | **Live** — the wall clock, and the drift |
| `brand.blue` | `#2C98FF` | **The engine** — the graph, the replay, determinism |
| `brand.indigo` | `#5D78FF` | Where they meet (`book`, `quote`, the cycle clock) |

That split is not decoration; it is the argument. The hook's two rules are
magenta and blue drifting apart, and the payoff converges them into a single
rule painted in `brand.gradient` — the same magenta-to-blue sweep the logo is
drawn in. Scene 3 gives one branch each colour and makes `book` and `quote` the
indigo between them; scenes 4 and 5 colour the engine-time column blue for the replay
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
| 3 | DAG | ✓ | The diamond, one clock notch per full cycle, and the nitro timings |
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

### The performance claim

Scene 3's numbers are **measured, not quoted**. `scripts/capture-bench.sh` runs
the committed `fanout` group from
[`benches/tiers.rs`](../../crates/wingfoil/benches/tiers.rs), reads criterion's
own estimates, and writes `assets/bench.json`; `src/bench.ts` renders from that
file. Nothing on screen is typed by hand, and re-measuring is one command.

`fanout` is a ticker and counter fanning out to 10×10 maps and recombining
through a 10-way merge — 103 nodes, the same fan-out-and-recombine *shape* the
scene animates beside it.

**The shape matches; the size does not, and the screen says so.** The diagram is
five boxes, and the `top_of_book` example behind it is twelve nodes
(`g.snapshot().nodes.len()`). 103 is the *benchmark's* node count, so the panel
reads `fanout benchmark · 103 nodes` and the narration says "benchmark graph" —
without that, a viewer reads the figure as describing the graph they are looking
at, which would be wrong by an order of magnitude.

| Tier | Per cycle | Per node-cycle |
| --- | --- | --- |
| interpreted | 1.57 µs | 15.2 ns |
| nested island | 138.8 ns | 1.35 ns |
| **nitro compiled** | **32.0 ns** | 0.31 ns |

Three deliberate omissions:

- **No third-party comparison.** An earlier cut carried "1610× tokio async
  streams", which is a real published figure on a depth-10 branch/recombine
  sweep — a workload built to expose per-path propagation, so it flatters
  wingfoil by construction. A ratio like that means nothing without its
  workload, a frame of video cannot carry that much fine print, and this
  audience is right to read a bare 1610× as a strawman. The tiers stand on
  their own.
- **No `legacy` bar**, though the bench measures one. It is an internal
  regression baseline, not a claim.
- **Not the per-node-cycle figure.** 0.31 ns per node-cycle is what the numbers
  divide to, and it is *too* good to lead with: at roughly one CPU cycle per
  node it invites the correct suspicion that the optimiser flattened part of a
  synthetic benchmark. Per *graph cycle* is the honest frame, and it is the one
  a reader can picture.

> **Absolute times are machine-specific.** These were measured on the shared
> cloud sandbox that rendered the video. `benches/README.md` is explicit that
> benchmarks want a quiet host, and a noisy one tends to punish the interpreted
> tier hardest — which inflates the ratio. Treat 49× as indicative and re-run
> `scripts/capture-bench.sh` on a quiet machine before leaning on it.

> **A discrepancy worth knowing about, not introduced here.** The root
> `README.md` gives engine overhead as **~27 ns** per node cycle;
> `benches/README.md` gives **~20 ns** in three places, backed by a measured
> slope. One of the two is stale. The video no longer quotes either, but the
> published pair should be reconciled.

### What scenes 4 and 5 actually show

This is the one place the film's design was decided by the engine rather than by
taste, so it is worth stating plainly.

`RunMode::HistoricalFrom(NanoTime::ZERO)` makes engine time pure logic, so the
replay stamps message time (`0.021_311 …`). `RunMode::RealTime` makes engine time
the **wall clock**, so the same quotes stamp as epoch nanoseconds
(`1,786,747,176.375_579 …`). The timestamps are *not* identical across modes, and
the video does not claim they are.

What is identical — and asserted by the capture script over the whole run, not
just the six rows shown — is the **quotes and their order**. So scene 5 renders
the real live capture with the clock column dimmed to the brand's magenta and the
quote held bright, and the narration says only what is true: same values, same
order, only the clock changes. The eye compares the bright halves across the two
scenes and finds them the same; the dim halves differ, which is exactly what a
wall clock is supposed to do.

> **One caveat worth knowing.** A live run reads the book at *cycle* boundaries,
> so under heavy load it can coalesce two updates that a replay resolves
> separately — fewer quotes, not different ones. That is honest behaviour for a
> live system rather than a bug, but it does mean the capture's equality assert
> is the thing standing behind the video's claim. If it ever fails, re-run on a
> quieter machine before changing any wording.

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
