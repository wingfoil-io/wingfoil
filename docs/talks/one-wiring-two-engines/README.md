# One wiring, two engines — conference talk

A 25-minute talk on wingfoil, in three parts:

1. **Intro** — who Jake is, the shape of problem wingfoil is for, what it is
   and how you use it, and a tour of what is in the box.
2. **Nitro** — performance, the wall we hit chasing it, the pattern that got us
   through, and what it bought.
3. **Next** — down the stack and up the stack, and where contributors could
   help.

Part 2 carries the argument: making node semantics an *associated function*
rather than a method on an object is what buys both Nitro engines, and why they
cannot drift.

> **Two, not three.** `Tier` has exactly two variants — `Interpreted` and
> `Compiled` — and `WINGFOIL_TIER` selects between those. A nested island is
> the compiled emission mounted with an interpreted boundary, not a third
> engine, which is why it lands *between* the two on every benchmark. An
> earlier draft of this deck said "three engines"; it overclaimed against the
> project's own type, and a Rust audience is precisely the room that checks.

The deck is [`index.html`](index.html) — reveal.js, entirely offline, 28 slides.

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

Two slides are the **trim valve**, in the order to spend them: **6** (the order
book) buys credibility but carries no step of the argument, and **8** (three
languages) is a 45-second aside. Dropping both recovers about two minutes and
breaks nothing.

**Prose is deliberately thin** — about 1,400 words across 28 slides, half what
an earlier draft carried. Anything you are going to *say* should not also be
written down: the roadmap of the talk, the fuller bio, caveats and transitions
all live in the speaker notes instead. If you find yourself reading a slide
aloud, that slide has too much on it.

### Presenting from a laptop

Two displays, **extended not mirrored**. Fullscreen the deck (<kbd>F</kbd>) on
the projector, then press <kbd>S</kbd> — a second window opens with the current
slide, the next slide, a timer and the notes. Drag that to your laptop screen.

Test it at the venue before you start, because two things bite:

- **The popup blocker.** <kbd>S</kbd> opens a window; a browser set to block
  popups swallows it silently. Allow popups for the page first. Verified
  working from `file://` in Chromium — no server needed.
- **Mirrored displays.** If the laptop mirrors the projector, the audience sees
  your notes. Check before the room fills.

**Printed fallback:** [`speaker-notes.pdf`](speaker-notes.pdf) — one thumbnail
plus its notes per slide, nine A4 pages. Worth having on paper if the venue is
unfamiliar.

### The two committed PDFs

Both are checked in so they can be downloaded without a checkout:

| | | Regenerate with |
|---|---|---|
| [`deck.pdf`](deck.pdf) | the slides, notes excluded — 28 pages | open `index.html?print-pdf` and print to PDF |
| [`speaker-notes.pdf`](speaker-notes.pdf) | thumbnail + notes per slide — 9 pages | `node build-notes.js` |

They are **derived files, and they lie the moment you edit a slide.**
`check.sh` compares their timestamps against `index.html` and fails if either
is older, so a stale PDF cannot quietly reach the repo — but only if you
actually run it.

### Exporting a PDF

Append `?print-pdf` to the URL and print to PDF (Chrome or Chromium; set
margins to none and enable background graphics). **One page per slide, 28
pages — if you get more than 28, stop and run `check.sh`**: extra pages mean
markup outside `.slides`, not a printing problem.

## Where every number and every output on these slides came from

House rule, same as the examples: **sample output is real** — run it and paste
what it prints. Nothing on these slides was invented or rounded into shape.
Captured 2026-08-24 in this repo:

| Slide | Claim | Source |
|---|---|---|
| 1, 2, 28 | the house title page, logo, hex graphic | extracted from the LDN Talks Sept 2025 deck's page 1 — the authentic embedded assets, date rolled to August 2026 |
| 2, 28 | the QR | `qr-repo.svg`, generated offline for `https://github.com/wingfoil-io/wingfoil` and verified to decode both as generated and as rendered on the slide |
| 3 | `sample` / `throttle` / `window` / `buffer` / `distinct` / `merge` / `split` | all real methods on `StreamOps` in [`fluent.rs`](../../../crates/wingfoil/src/fluent.rs) |
| 4 | the graph figure | abstract, not a claim about any particular wiring. Two live sources fan out and recombine onto two outputs; the lit subgraph is chosen over the drawn topology so every split and join in it is a real edge of the picture |
| 5 | `tick 1 … tick 5` | `cargo run -p wingfoil --example hello_graph` |
| 6 | the code | verbatim from the [`order_book` example README](../../../crates/wingfoil/examples/core/order_book/README.md); chart is that example's own `aapl.svg`. The run's counts and timing were on this slide and were real (`--release --features csv --example order_book`); they came off it because the slide carries title, code and chart only |
| 7 | both terminal panels | `cargo run --release -p wingfoil --example top_of_book` and the same with `-- realtime`, run in this tree. The quote columns are identical across the two by construction — the live run is a prefix of the replay |
| 7 | the pacing of the live panel | the captured epoch timestamps' real gaps, slowed 12× so the pauses are visible — stated in the panel's own title bar |
| 7 | the code excerpt | condensed from [`top_of_book/main.rs`](../../../crates/wingfoil/examples/core/top_of_book/main.rs) — the example picks its run mode with a `match` on the command-line argument and calls `run` once; the slide shows that as an `if` to fit four lines |
| 8 | the sixteen I/O adapters | the adapter table in [`src/adapters/CLAUDE.md`](../../../crates/wingfoil/src/adapters/CLAUDE.md) — `augurs`, `cache`, `market` and `statistics` are not I/O |
| 8 | the three patterns, and the example named for each | busy loop = `Activation::ALWAYS` ([`iceoryx2/read.rs`](../../../crates/wingfoil/src/adapters/iceoryx2/read.rs)); worker thread = [`source_at_start`](../../../crates/wingfoil/src/fluent.rs) + a spawned thread (`zmq.rs`); async = [`produce_async`](../../../crates/wingfoil/src/async_source.rs) (`kdb.rs`) |
| 8 | the latency column | the cost **the pattern adds**, not end-to-end: a spin is one poll per cycle (tens of ns), a channel hop and a task wake are microsecond-scale. Note [`iceoryx2/mod.rs`](../../../crates/wingfoil/src/adapters/iceoryx2/mod.rs) documents `Spin` delivery itself at **~1–5 µs** — so "nanoseconds" here is the scheduling overhead, not the transport |
| 9 | the cube | orthographic projection; its three faces are the three surfaces — [`crates/wingfoil-python/`](../../../crates/wingfoil-python/) (the wheel) and [`crates/wingfoil-wasm/`](../../../crates/wingfoil-wasm/) + [`js/`](../../../js/) (the browser client, decoding with the server's own codec) |
| 11 | **12.953 µs**, value 2<sup>127</sup> | `--release --example breadth_first` (the `topological_sort` example) |
| 12 | slopes 2.01× / 1.94× | [`benches/topological_vs_per_path/`](../../../crates/wingfoil/benches/topological_vs_per_path/); chart is that suite's `headline_log.png` |
| 14 | the three walls, and the diagnosis | [`docs/blog/rearchitecting-wingfoil.md`](../../blog/rearchitecting-wingfoil.md) |
| 16 | the `Op` trait | [`docs/wingfoil-architecture.md`](../../wingfoil-architecture.md) |
| 17 | the legacy/now code, all four panels | verbatim from [`docs/migration.md`](../../migration.md) — the `MutableNode` and `Op` forms of the same node, and the before/after wiring |
| 18 | two engines, island as the seam | [`tier.rs`](../../../crates/wingfoil/src/tier.rs) — `enum Tier { Interpreted, Compiled }` |
| 19 | the `nitro!` wiring | verbatim from [`examples/core/dual_mode/main.rs`](../../../crates/wingfoil/examples/core/dual_mode/main.rs) |
| 20 | the expansion | abridged from the committed [`expanded/main.expanded.rs`](../../../crates/wingfoil/examples/core/dual_mode/expanded/main.expanded.rs) (1,730 lines); elisions marked |
| 21 | the two tier runs | `WINGFOIL_TIER=interpreted` and `=compiled`, same binary, `--example dual_mode` |
| 22 | the workload table, ~0.3 ns / ~12 ns, 4.4–37×, 0.56–0.84× | [`benches/README.md`](../../../crates/wingfoil/benches/README.md) |
| 23 | user op within 2.4% of a built-in | `benches/README.md`, "A user's op is not a second-class citizen" |
| 26 | the up-the-stack list, and fold/filter/join | [`docs/planning/trading-roadmap.md`](../../planning/trading-roadmap.md) §3 |
| 27 | **~20 contributors** | ⚠️ **unverified** — this clone is shallow (64 commits), so git shows only 5 non-bot authors. Confirm against GitHub before saying it aloud |

If you re-run any of these on different hardware the absolute times will move.
The **ratios** are the claim — say so from the podium, as slide 15's footnote
and its speaker notes both do.


## Motion

Two slides animate on arrival — the graph on slide 4 draws its live path, and
slide 9's two terminals fill at their two different pacings. Both are CSS
keyframes in `theme.css`, triggered by reveal's own `.present` class rather
than by fragments, so a slide plays itself when you land on it and costs no
extra clicker press. Leaving and coming back replays it.

The rule that keeps this safe to export: **every animation runs from a start
state into the resting state, never out of it.** The un-animated rendering is
therefore the finished picture, not a blank one — which is what makes
`?print-pdf` (which sets `no-anim` on the document) and `prefers-reduced-motion`
correct for free, with no second rendering path to keep in sync. If you add an
animation here, put the finished values in the ordinary rule and the starting
values in the keyframes, never the reverse; `deck.pdf` is the check.
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

The deck is a fixed 1280×800 stage, so content that overflows is silently
clipped rather than reflowed — which is exactly the failure you do not want to
discover on stage. Two failure modes are worth re-checking after any edit: a
slide taller than the stage, and a code block that scrolls inside its own box.

Both were verified with a headless Chromium pass over all 28 slides
(`Reveal.slide(i)`, then compare `scrollHeight` against the 800 px stage and
each `pre code`'s `scrollHeight` against its `clientHeight`). If you add a
slide, re-run that check rather than trusting the eye — the second failure mode
in particular looks fine until the last two lines are missing.

### Why the stage is 16:10, not 16:9

Decks default to 16:9. But 16:9 content on a 16:10 panel — 1920×1200,
1440×900, 2560×1600, essentially every laptop anyone presents from —
letterboxes with roughly 160 px of black top and bottom, using only 83% of the
screen. A 16:10 stage fills the same panel to 94%.

The cost is that a 16:9 projector now gets thin side bands instead (15% unused
against 8% before). That is the cheaper of the two losses, and it is the
screen you are *not* looking at while you present.

Measured across 1080p, three 16:10 laptop sizes, 4:3 and 21:9 — every 16:10
target sits at 6% unused. If you know in advance you are presenting to a 16:9
projector and nothing else, set `height: 720` back in `Reveal.initialize` and
re-run the layout check.

**Also: press <kbd>F</kbd>.** Browser chrome eats 100–150 px of height, and
reveal scales the stage to the viewport it is given, so a non-fullscreen window
shrinks the whole deck.

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
on the 1280×800 stage, code lands at ~21 px, and nothing drops below ~18 px.

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
