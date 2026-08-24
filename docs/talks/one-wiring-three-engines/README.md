# One wiring, three engines — conference talk

A 25-minute talk for a **Rust engineering** audience, arguing one thesis: that
making node semantics an *associated function* rather than a method on an
object is what buys the three Nitro execution tiers, and that the three tiers
cannot drift because there is only one copy of the semantics.

The deck is [`index.html`](index.html) — reveal.js, entirely offline, 20 slides.

## Presenting

Open `index.html` in any browser. No build step, no server, no network.

| Key | |
|---|---|
| <kbd>→</kbd> / <kbd>Space</kbd> | next slide |
| <kbd>←</kbd> | previous |
| <kbd>S</kbd> | **speaker view** — notes, timer, next-slide preview, in a second window |
| <kbd>Esc</kbd> | slide overview grid |
| <kbd>F</kbd> | fullscreen |
| <kbd>B</kbd> | black the screen |

Every slide carries speaker notes with its timing budget. The pacing target is
in the notes on slide 1: **if you are past slide 9 (the `Op` trait) at the
10-minute mark you are on time.** The content runs ~22.5 minutes against the
25-minute slot, so the slack is thin — buy room with the trim valve below
rather than by rushing act one.

Two slides are marked in their notes as the **trim valve** — 16 (contract
decisions) and 18 (what's around the engine). Cutting both to a sentence each
recovers about three minutes without breaking the argument, because neither
carries a step of the thesis.

### Exporting a PDF

Append `?print-pdf` to the URL and print to PDF (Chrome or Chromium; set
margins to none and enable background graphics). One page per slide, 20 pages.

## Where every number and every output on these slides came from

House rule, same as the examples: **sample output is real** — run it and paste
what it prints. Nothing on these slides was invented or rounded into shape.
Captured 2026-08-24 in this repo:

| Slide | Claim | Source |
|---|---|---|
| 2 | `sample` / `throttle` / `window` / `buffer` / `distinct` / `merge` / `split` | all real methods on `StreamOps` in [`fluent.rs`](../../../crates/wingfoil/src/fluent.rs) — the slide names no op that does not exist |
| 4 | `tick 1 … tick 5` | `cargo run -p wingfoil --example hello_graph` |
| 5 | 15,040 prices / 4,169 fills / **119.704 ms** | `--release --features csv --example order_book`. The chart is the example's own committed `aapl.svg` |
| 6 | **12.953 µs**, value 2<sup>127</sup> | `--release --example breadth_first` (the `topological_sort` example — the target keeps its historical name) |
| 7 | slopes 2.01× / 1.94×, ≈ 68 ns + 22 ns × depth, ~39× / ~134× at depth 10 | [`benches/topological_vs_per_path/README.md`](../../../crates/wingfoil/benches/topological_vs_per_path/); chart is that suite's committed `headline_log.png` |
| 9 | the `Op` trait | [`docs/wingfoil-architecture.md`](../../wingfoil-architecture.md) |
| 12 | the `nitro!` wiring | verbatim from [`examples/core/dual_mode/main.rs`](../../../crates/wingfoil/examples/core/dual_mode/main.rs) |
| 13 | the expansion | abridged from the committed [`expanded/main.expanded.rs`](../../../crates/wingfoil/examples/core/dual_mode/expanded/main.expanded.rs) (1,730 lines). **Elisions are marked**; no tokens were rewritten |
| 14 | the two tier runs | `WINGFOIL_TIER=interpreted` and `=compiled`, same binary, `RUST_LOG=info --example dual_mode` |
| 15 | the eight-workload table, ~0.3 ns / ~12 ns, 4.4–37×, 0.56–0.84× | [`benches/README.md`](../../../crates/wingfoil/benches/README.md) |
| 17 | user op within 2.4% of a built-in | `benches/README.md`, "A user's op is not a second-class citizen" |
| 19 | the three lessons | [`docs/blog/rearchitecting-wingfoil.md`](../../blog/rearchitecting-wingfoil.md) |

If you re-run any of these on different hardware the absolute times will move.
The **ratios** are the claim — say so from the podium, as slide 15's footnote
and its speaker notes both do.

## Checking the layout

The deck is a fixed 1280×720 stage, so content that overflows is silently
clipped rather than reflowed — which is exactly the failure you do not want to
discover on stage. Two failure modes are worth re-checking after any edit: a
slide taller than the stage, and a code block that scrolls inside its own box.

Both were verified with a headless Chromium pass over all 20 slides
(`Reveal.slide(i)`, then compare `scrollHeight` against the 720 px stage and
each `pre code`'s `scrollHeight` against its `clientHeight`). If you add a
slide, re-run that check rather than trusting the eye — the second failure mode
in particular looks fine until the last two lines are missing.

## What is vendored, and why

`vendor/reveal/` holds reveal.js 5.1.0 — `reveal.js`, `reveal.css`,
`reset.css`, and the notes plugin — under its MIT `LICENSE`, kept beside the
files it covers. It is vendored rather than pulled from a CDN so the deck works
with no network at a venue, which is the whole point of a self-contained deck.

**highlight.js is deliberately not vendored.** Its bundle is 920 KB of ~190
language grammars, to syntax-highlight a dozen Rust snippets. `index.html`
carries a ~40-line Rust tokenizer instead, covering exactly the constructs this
deck uses. If you add a snippet using syntax it does not know, the worst case
is that a token renders unstyled — never that the code renders wrong.

`theme.css` is ours. It sizes the deck from the code outward: the root is 34 px
on the 1280×720 stage, code lands at ~21 px, and nothing drops below ~18 px.
