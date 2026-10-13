# Talks

Conference and meetup decks, one directory each. A deck lives here when it has
actually been prepared for a real audience — not as a place to draft slides
speculatively.

Each directory is **self-contained and offline**: open its `index.html` and
present, with no build step, no server and no network. Anything a deck depends
on is either inside its directory or referenced by relative path into the
repository it is about, so a deck never goes stale against a chart or an
example it cites without that showing up as a broken link.

| | Audience | Runtime | |
|---|---|---|---|
| [`one-wiring-two-engines/`](one-wiring-two-engines/) | Rust engineers | 25 min | Semantics as an associated function, and the two Nitro engines that fall out of it |

## Adding a deck

Give it a directory named after the talk, an `index.html`, and a `README.md`
covering how to present it, how to export a PDF, and — the part that matters —
**where every number and every sample output on the slides came from**.

That last section is not bookkeeping. The same house rule that governs example
READMEs governs slides: output is real, captured by running the thing, never
written from memory or rounded into a nicer shape. A talk is the most public
claim this project makes, and the provenance table is what lets the next person
re-check it rather than trust it.

Slides are a fixed stage, so overflowing content is clipped rather than
reflowed. Verify a new or edited slide against the stage height before calling
it done — `one-wiring-two-engines/README.md` describes the headless check
used there, including the failure mode that looks fine until you notice the
last two lines are gone.
