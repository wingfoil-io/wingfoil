# One wiring, two engines — conference talk

A 25-minute talk for a **Rust engineering** audience, arguing one thesis: that
making node semantics an *associated function* rather than a method on an
object is what buys both Nitro execution tiers, and that the two cannot drift
because there is only one copy of the semantics.

> **Two, not three.** `Tier` has exactly two variants — `Interpreted` and
> `Compiled` — and `WINGFOIL_TIER` selects between those. A nested island is
> the compiled emission mounted with an interpreted boundary, not a third
> engine, which is why it lands *between* the two on every benchmark. An
> earlier draft of this deck said "three engines"; it overclaimed against the
> project's own type, and a Rust audience is precisely the room that checks.

The deck is [`index.html`](index.html) — reveal.js, entirely offline, 22 slides.

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
10-minute mark you are on time.** The content runs ~23 minutes against the
25-minute slot, so this is a full deck — budget Q&A separately, or trim.

Two slides are the **trim valve**, in the order to spend them: **5** (the
order book) buys credibility but carries no step of the argument, and **18**
(three languages) is a 45-second aside. Dropping both recovers about two
minutes and breaks nothing.

### Exporting a PDF

Append `?print-pdf` to the URL and print to PDF (Chrome or Chromium; set
margins to none and enable background graphics). **One page per slide, 22
pages — if you get more than 22, stop and run `check.sh`**: extra pages mean
markup outside `.slides`, not a printing problem.

## Where every number and every output on these slides came from

House rule, same as the examples: **sample output is real** — run it and paste
what it prints. Nothing on these slides was invented or rounded into shape.
Captured 2026-08-24 in this repo:

| Slide | Claim | Source |
|---|---|---|
| 3 | `sample` / `throttle` / `window` / `buffer` / `distinct` / `merge` / `split` | all real methods on `StreamOps` in [`fluent.rs`](../../../crates/wingfoil/src/fluent.rs) — the slide names no op that does not exist |
| 5 | `tick 1 … tick 5` | `cargo run -p wingfoil --example hello_graph` |
| 6 | 15,040 prices / 4,169 fills / **119.704 ms** | `--release --features csv --example order_book`. The chart is the example's own committed `aapl.svg` |
| 7 | **12.953 µs**, value 2<sup>127</sup> | `--release --example breadth_first` (the `topological_sort` example — the target keeps its historical name) |
| 8 | slopes 2.01× / 1.94×, ≈ 68 ns + 22 ns × depth, ~39× / ~134× at depth 10 | [`benches/topological_vs_per_path/README.md`](../../../crates/wingfoil/benches/topological_vs_per_path/); chart is that suite's committed `headline_log.png` |
| 10 | the `Op` trait | [`docs/wingfoil-architecture.md`](../../wingfoil-architecture.md) |
| 12 | that there are two engines, and the island is the seam | [`tier.rs`](../../../crates/wingfoil/src/tier.rs) — `enum Tier { Interpreted, Compiled }`, read straight off the type |
| 13 | the `nitro!` wiring | verbatim from [`examples/core/dual_mode/main.rs`](../../../crates/wingfoil/examples/core/dual_mode/main.rs) |
| 14 | the expansion | abridged from the committed [`expanded/main.expanded.rs`](../../../crates/wingfoil/examples/core/dual_mode/expanded/main.expanded.rs) (1,730 lines). **Elisions are marked**; no tokens were rewritten |
| 15 | the two tier runs | `WINGFOIL_TIER=interpreted` and `=compiled`, same binary, `RUST_LOG=info --example dual_mode` |
| 16 | the eight-workload table, ~0.3 ns / ~12 ns, 4.4–37×, 0.56–0.84× | [`benches/README.md`](../../../crates/wingfoil/benches/README.md) |
| 17 | user op within 2.4% of a built-in | `benches/README.md`, "A user's op is not a second-class citizen" |
| 18 | the four `Activation` modes; iceoryx2 Spin/Threaded/Signaled | [`op.rs`](../../../crates/wingfoil/src/op.rs) and the [iceoryx2 example README](../../../crates/wingfoil/examples/adapters/iceoryx2/) — its own polling-mode table |
| 21 | `Cfg` / `State` are opaque associated types the engine cannot inspect | the [`Op` trait](../../wingfoil-architecture.md) itself — the claim is read off the signature |
| 22 | the "build around it" list, and that position/risk/PnL are fold/filter/join | [`docs/planning/trading-roadmap.md`](../../planning/trading-roadmap.md) §3 — the functional gap, in its own rough effort order |
| 1 | the house title page, logo and hex graphic | extracted from the LDN Talks Sept 2025 deck's page 1 — the authentic embedded assets, date rolled to August 2026 |
| 2, 22 | the logo | `logo.png` — the authentic mark, extracted from the LDN Talks deck; see "The logo and the hex graphic" below |
| 2, 22 | the QR | `qr-repo.svg`, generated offline for `https://github.com/wingfoil-io/wingfoil` and verified to decode both as generated and as rendered on the slide |

If you re-run any of these on different hardware the absolute times will move.
The **ratios** are the claim — say so from the podium, as slide 15's footnote
and its speaker notes both do.

## Checking the deck after an edit

Run [`check.sh`](check.sh) first. It compares `<section>`/`</section>` counts,
requires exactly one notes block per slide, and requires exactly one
`</div></div>` closer at line start.

That last one sounds fussy and is the important one. An edit that splices
slides in or out can leave stale `<section>` blocks **outside** the `.slides`
container. Reveal drops them from its slide list, so the deck still presents
correctly and headless screenshots still look right — but the orphans render
as stray text on the live screen and spill the PDF export onto extra garbled
pages. It happened once, cost 350 lines of duplicated slides, and no
screenshot check caught it.

## Checking the layout

The deck is a fixed 1280×720 stage, so content that overflows is silently
clipped rather than reflowed — which is exactly the failure you do not want to
discover on stage. Two failure modes are worth re-checking after any edit: a
slide taller than the stage, and a code block that scrolls inside its own box.

Both were verified with a headless Chromium pass over all 22 slides
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

### The palette, and what is approximate about it

The colours follow [wingfoil.io](https://www.wingfoil.io/): pure-black ground,
white headlines, periwinkle body copy, and the magenta→cyan gradient of the hex
logo mark, with the site's blue used for links.

**They were sampled off a screenshot of the site, not read from its
stylesheet** — this session's network egress policy blocks `wingfoil.io`, so
neither `curl` nor a fetch tool could reach it. Treat them as close rather than
exact, and replace the values in the `:root` block from the real CSS when
someone can read it. The two inline SVGs (the tick lanes on slide 2 and the
structure diagram on slide 20) carry brand hexes directly, so they need
updating with it.

The deck follows the site's restraint: it is near-monochrome, and emphasis is
carried by **brightness** — white against periwinkle — rather than by colour.

**Magenta is a highlight, not a text colour.** It earns its place on the
gradient rule, the punch bar, the logo, a card border, and the one stat or
phrase per slide worth pointing at — nowhere else. Two drafts got this wrong
in different ways: the first coloured every `<strong>`, the second left it on
headings, bullet markers, table headers and all three stat figures at once.
Both read as *purple slides* rather than dark slides with an accent, and in
both the punch line stopped being the loudest thing on the slide, which is its
only job. `.stat .v.hi` exists as the deliberate opt-in for the one number you
do want to point at.

### The logo and the hex graphic — the real assets

`logo.png` and `hexwave.png` were extracted from the LDN Talks September 2025
deck, which embeds both as images. They are the authentic assets, not
reconstructions: the logo came out at 1024×1024 with a separate alpha mask,
which had to be composited back on before the transparency was right, then
trimmed to its bounding box and scaled to 233×320.

An earlier `logo-reconstructed.svg` — traced by eye because egress blocks
wingfoil.io — is deleted. It was wrong in substance as well as detail: the real
mark is a hexagonal *spiral* of nested rings, not the hexagon-with-a-counter
the trace guessed at.

Slide 1 is the house title page lifted from that same deck and rebuilt in HTML
rather than pasted as an image, so the date stays editable — currently
**August 2026**.

`qr-repo.svg` is generated rather than fetched, so the deck stays offline: a
33×33 module QR at error-correction M, emitted as one path of unit squares so
it is crisp at any projected size. It is dark modules on a white plate because
that is what a phone camera expects, and the deck's own background is
near-black. It was checked twice — decoded from the generator, and decoded
again out of screenshots of both rendered slides, which is the path that
actually matters. **If the repo URL ever moves, regenerate it**; a stale QR
fails silently and in front of an audience.
