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

### Why there is no performance claim

The film makes no speed claim, and that is a decision rather than an omission.

Four versions of one were built and all four were withdrawn. The last one was
measured, on the example itself, at a scale worth measuring — and it was still
wrong, for a reason that rules out the whole category. Decomposing a 156 ms
replay of the hour:

| | Time | Share |
| --- | --- | --- |
| `lobster` order book (third-party crate) | ~56 ms | 36% |
| Writing 15,387 lines to stdout | ~80 ms | 51% |
| **wingfoil's engine** | **~23 ms** | **15%** |

**A number measured on that graph is 85% a measurement of things that are not
wingfoil.** The confirming test came from the other side: swapping `println!`
for a 64 KB `BufWriter` moved the "engine" figure by 30% (156 ms → 110 ms)
without touching the engine at all. If the sink can move your number by a third,
the number was never about the framework.

That is not a wingfoil-specific problem — it is what a realistic workload looks
like. Per-message payload work and I/O dominate; engine dispatch is the small
term. The honest consequence is that a marketing frame is the wrong place for a
throughput figure, because the caveats needed to make it true are longer than
the claim.

For the record, since the measurements exist: `log` and `tracing` are both
*slower* than `println!` when enabled (169 ms and 174 ms against 156 ms) —
each adds a timestamp, level and target on top of the same syscall-per-record —
and both are essentially free when disabled (~89 ms against an 86 ms
no-output floor), because they check the level before evaluating format
arguments. That is the argument for `logged` as a tap you leave wired and
switch off, and for `for_each_mut` with a buffered writer as a real output edge.

The claims the film does make — one definition, identical quotes in identical
order, only the clock differs — are properties it can actually support, and the
capture script asserts them on every build.

The engine's own numbers, with the method and caveats they need, live in
[`benches/README.md`](../../crates/wingfoil/benches/README.md). That is the
right place for them.

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

## Recording your own voiceover

Scene lengths have always been *derived* from audio, never typed, so replacing
the voice re-times the film automatically. `scripts/import-voice.py` is that
path:

```sh
# 1. See what to record — the lines, and the filename each belongs in.
scripts/import-voice.py --script

# 2. Record ONE continuous take, reading the lines in order, leaving about a
#    second of silence between them.

# 3. Cut it on those pauses. Without --to it only reports, so you can check
#    the cuts before anything is written.
scripts/split-voice.py --from take.wav
scripts/split-voice.py --from take.wav --to ~/vo

# 4. Re-time the film around them.
scripts/import-voice.py --from ~/vo

# 5. Check a reader can keep up, rebuild the subtitles, render.
python3 scripts/check-pacing.py
python3 scripts/build-srt.py
npm run render
```

Recording seven separate files works too — skip step 3 and name them
`hook.wav`, `code.wav`, `dag.wav`, `historical.wav`, `realtime.wav`,
`payoff.wav`, `cta.wav`. The splitter exists because one take is a much easier
thing to record.

### Splitting one take

`split-voice.py` finds runs of speech separated by at least `--gap` seconds of
near-silence, and refuses to write anything unless it finds exactly as many as
there are scenes. It prints each segment beside the line it will become, so a
mis-detection is visible before it reaches a render:

```text
take.wav: 48.0s, found 7 segment(s)

  1.   0.04s →   5.08s  ( 5.04s)  hook
      Your backtest and your live system are usually two different codebases. …
  2.   6.74s →  13.40s  ( 6.66s)  code
      You describe it once, in Rust. For example: an order book, fanned out to…
```

If the count is wrong, the two knobs are `--gap` (default 0.9s — raise it if a
mid-sentence pause is being cut on, lower it if your gaps between lines are
short) and `--floor` (default −40 dBFS — raise toward −30 in a noisy room).
Neither writes anything until the segments line up.

Nothing in `src/` is touched. The importer measures each wav, copies it into
`public/audio/`, and rewrites `assets/narration.json`.

**Uncompressed PCM wav.** Any sample rate, mono or stereo. If you record to
m4a/mp3, convert first (`ffmpeg -i take.m4a hook.wav`).

**Read the script as written, or edit `scripts/script.json` to match what you
said** — the caption text comes from that file, not from your audio, so the two
drift apart if you ad-lib.

**Level them before importing.** The importer copies your wavs verbatim, so
mixing is on you; piper's output is quiet and even, and a hotter recording will
make the film's loudness jump.

**Expect the pacing gate to have opinions.** It fails the build over 15.5
characters per second, and a natural reading pace is usually faster than the
synthesised one — the synthesised build also pads every scene (a lead-in, a
tail, and a gap between sentences) that a trimmed recording does not have. When
it complains: slow the delivery, shorten the line, or pass `--hold`.

`--hold` only widens the *last* sentence of a scene, though, so it cannot fix a
short opening line that the character-weight apportioning gave too little time.
That one needs real word timings — see below.

`--hold` keeps the per-scene `hold` values from `script.json` (extra on-screen
time after the voice stops, for cards that need reading time). It is off by
default because a human recording usually carries its own tail.

### Word-level karaoke

Without help, the importer apportions each scene's known text across the
measured duration by character weight. That is fine for a caption band, but the
highlight will drift within a sentence, because a single wav cannot give up its
sentence boundaries the way piper's sentence-at-a-time synthesis does.

For real word timings, drop a `<scene>.words.json` beside the wav:

```json
[{"word": "Run", "start": 0.15, "end": 0.34}, ...]
```

which is the shape [`@remotion/install-whisper-cpp`](https://remotion.dev/docs/install-whisper-cpp)
produces. Any scene with one uses it; the rest fall back to apportioning, so you
can transcribe just the scenes that need it.

## Rendering notes

Remotion downloads its own headless shell on first render. Do **not** point it at
the container's `/opt/pw-browsers/chromium` — full Chromium rejects the
old-headless mode Remotion uses and the launch fails.

Type is set in Liberation Sans and DejaVu Sans Mono, both present on the render
host, so no font is fetched at build time and the render stays offline.

## Posting

`copy.md` holds the post text, the first comment, and the upload checklist.
Upload the MP4 **natively** — a native video gets far more reach than a link out.
