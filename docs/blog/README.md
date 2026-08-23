# Blog

Long-form writing about the engine — the reasoning behind a change, at the
length an argument needs rather than the length a reference entry allows.

| | |
|---|---|
| [**Rearchitecting Wingfoil: one definition, three engines**](rearchitecting-wingfoil.md) | Why the engine was rewritten rather than refactored, the `Op` decision everything else follows from, and what the rewrite measured |

## What belongs here

A post is for an audience that has not read the repo: it can assume interest
but not context, and it argues a case rather than recording one. That is what
separates it from the three directories next door.

| | goes to |
|---|---|
| Answers a question once, for people working in the tree | [`decisions/`](../decisions/) |
| Tracks a body of work over time | [`planning/`](../planning/) |
| Tells someone what changed in a release and what to do about it | [`release-notes/`](../release-notes/) |
| Makes an argument to someone who does not work here | **`blog/`** |

Two rules the rest of `docs/` already lives by, and which bite harder in a post
that outlives the tree it describes:

- **Every number is quoted from something that gets recaptured.** Prefer
  [`benches/README.md`](../../crates/wingfoil/benches/README.md) over a decision
  record's frozen table: both are true, but only one of them is re-measured, and
  a post is the document most likely to be read a year late.
- **Snippets are copied from a working example, not written for the prose.**
  The house rules that apply to examples — stream output rather than collecting
  it, no `accumulate()` outside a test — apply here for the same reason, with
  the added risk that a reader will copy what a post shows and never see the
  example that does it properly.
