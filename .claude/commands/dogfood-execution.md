Dogfood the **execution layer** (`wingfoil::trading::execution`) as its
*users* would meet it, with a team of small-model agents, and harvest what made
it hard to use or wrong. `$ARGUMENTS`, if given, narrows or overrides the fixed
team in step 1; blank runs the standard eight.

This is the sibling of `/dogfood`, and differs from it in one way that drives
everything else: `/dogfood` measures **contributor** friction (skills, CI
gates, the example rules), so its agents edit the repo. This skill measures
**API** usability and quality, so its agents never touch the repo — each
builds a small downstream crate *outside* the workspace that depends on
`wingfoil` by path, and may use only what a crates.io user could: `pub` items,
rustdoc, `docs/execution-layer.md`, and the examples. Read `/dogfood` once;
what is not restated here (the Haiku rationale, the review discipline, the
lessons of its first run) applies unchanged.

The product is two lists, kept separate because they are fixed differently:

- **Usability friction** — the API made a correct program harder to write than
  it needed to be: discoverability, naming, generic bounds, boilerplate,
  missing re-exports, docs that are thin or wrong.
- **Quality defects** — the layer did something its docs say it does not, or
  panicked, or accepted a value it should refuse. Each must arrive with a
  **failing repro** the reviewer can run. A defect with no repro is a
  usability item at best.

## The model is Haiku. Do not ask, do not offer alternatives.

Same reasoning as `/dogfood`: a strong model infers past a bad signature or a
missing doc line and reports nothing. A weak model writing against the public
surface is a fair proxy for a user new to the layer. **An unfinished program is
an expected, acceptable outcome** — say so in every prompt. "Could not find how
to get fills out of the OMS session; here is what I tried" is the data.

## 1. The team — eight user stories, fixed composition

Each agent gets a **user story**, phrased the way a trading developer would
state it — *what* they want, never *which type does it*. Naming the type in the
prompt measures nothing: finding it is half of what is under test.

The standard eight, spread so every module is reached and every story lands on
at least one node trait (`OmsLoopOps`, `PositionOps`, `ReconcileOps`,
`KillSwitchOps`, `AlgoOps`, `FixOps`):

1. **Quote one instrument and stop on a loss.** Two-sided quotes around a mid,
   against the test venue; fills into a position; halt everything when the day's
   loss crosses a limit. *(The `strategy_loop` path, written from scratch.)*
2. **Write your own venue.** Implement the swap point for a toy venue that
   fills every order at its limit price immediately, and run a quoter against
   it **without** `execution-testing`. *The single most valuable story* — it is
   the only one that exercises the layer from the venue side, which is what
   every real integration does. Ask specifically: was the contract a `Venue`
   must honour stated anywhere, or did you reverse-engineer it from `SimVenue`?
3. **Work a large order as TWAP.** Split a parent order into clips over an
   hour of engine time, fold the fills into a position, print average price.
4. **Catch a position break.** The venue's holdings disagree with the fills
   seen; detect it, and say what the layer does about it past the grace.
5. **Your own instrument type.** An `enum` of instruments — one linear equity,
   one index future with a multiplier, one inverse future — valued in one
   `Book`. Probes the `Instrument` bounds and `Measure`.
6. **Respect the venue's limits.** A bursty strategy against an order-rate
   budget and a message-to-trade ratio: what is deferred, what is dropped, and
   can the user observe which?
7. **Put the OMS in front of FIX.** Wire the OMS to a FIX-shaped venue through
   the bridge and replace an order. Ask: was it clear which id is the user's
   and which is the venue's?
8. **The failure paths.** Send an order over a per-contract cap, a `NaN`
   price, a cancel for an order that does not exist, a kill-switch `clear`
   while still in breach. Record for each: refused, silently accepted, panic,
   or run abort — and whether the docs said which. *This is the quality
   agent*; its output is mostly repros.

### Re-derive before spawning — stories rot like gap lists

The layer is moving (see `git log --oneline -- crates/wingfoil/src/trading/execution`).
Before spawning, confirm each story is still reachable from the public surface
and still names a real capability:

```bash
cd /home/user/wingfoil/crates/wingfoil
# The node traits a user wires with — every story should land on one.
grep -rhn 'pub trait [A-Za-z]*Ops' src/trading/execution
# The swap point and the test venues.
grep -rn 'pub trait Venue\|pub struct SimVenue\|pub struct FixVenue' src/trading/execution
# What the module map promises (and what it says the layer is NOT —
# don't assign a story the docs rule out, e.g. RFQ or a venue simulator).
sed -n '/^\/\/! # What it is/,/^pub mod/p' src/trading/execution/mod.rs
```

A capability the docs say is out of scope is not a story — unless the point is
to test whether a user can *tell* it is out of scope, in which case say so in
the prompt.

## 2. Set up — scratch crates, not worktrees

Agents do not edit the repo, so they need no worktree. Each gets a crate:

```bash
U=<scratchpad>/userland
mkdir -p $U
for n in quote venue twap recon instr limits fix fail; do
  cargo new --quiet --vcs none $U/$n
  cp /home/user/wingfoil/Cargo.lock $U/$n/   # see "Protect the disk"
done
```

Each crate's `Cargo.toml` gets exactly:

```toml
[dependencies]
wingfoil = { path = "/home/user/wingfoil/crates/wingfoil", features = ["execution"] }
anyhow = "1"
```

Add `execution-testing` for stories 1, 3, 4, 6, 7, 8 (they need a test venue).
Story 2 **must not** have it — that is the test. Put an empty `[workspace]`
table in each so cargo does not walk up into the wingfoil workspace.

### Protect the disk

1. **Copy the workspace `Cargo.lock` in** (above). A fresh crate otherwise
   resolves newest-compatible versions, which differ from the lock and miss
   the shared cache — eight full dependency builds. Cargo trims the copied lock
   to what the crate uses.
2. **Shared target dir**: every prompt sets
   `export CARGO_TARGET_DIR=/home/user/wingfoil/target CARGO_INCREMENTAL=0`.
   `Blocking waiting for file lock` is expected; wait.
3. **Pre-warm** while writing prompts:
   `CARGO_INCREMENTAL=0 cargo check -p wingfoil --features execution-testing`.
4. **Pre-build the docs once** for the agents to read:
   `cargo doc -p wingfoil --no-deps --features execution-testing` → they read
   `target/doc/wingfoil/trading/execution/`. One doc build, not eight.

## 3. Spawn the team

All eight in **one message**, `model: "haiku"`, no isolation needed.

### Rules every agent prompt must carry

- **You are a user, not a contributor.** Your code lives in `<crate path>`.
  Do not edit anything under `/home/user/wingfoil`.
- **What you may read**: rustdoc under `target/doc/wingfoil/`,
  `docs/execution-layer.md`, `crates/wingfoil/examples/trading/`, and
  `docs/wingfoil-architecture.md` for the engine. **Not** `crates/wingfoil/src/`
  — a user reading source to work out a signature is itself the finding, so if
  you get stuck enough to want to, write down what you were looking for and
  *then* look, and log it as friction.
- **Allowed**: `cargo check`, `cargo run`, `cargo test`, `cargo fmt` in your
  own crate. **Forbidden**: `--release`, `cargo doc` (already built), anything
  run inside `/home/user/wingfoil`.
- **Run under `RunMode::HistoricalFrom(NanoTime::ZERO)`** so what you report
  is reproducible, and run the binary under `timeout 60` — a graph that
  livelocks in engine time burns CPU without ending (run 1 lost 13 minutes
  to one).
- **Try the rustdoc before an example**, and record whether you opened an
  example before your first compile. A low compile count earned by copying
  an example measures the example, not the API.
- **Count your compile attempts** — every `cargo check`/`run` that failed to
  compile — and record it. It is the closest thing this run has to a
  usability metric, and comparing it across stories shows which part of the
  layer is hardest to get right.
- **A suspected bug needs a repro**: a `#[test]` in your crate that fails,
  with a comment quoting the doc line it contradicts. No doc line, no bug —
  log it as "behaviour I could not predict from the docs" instead.

### The report

Each agent writes `<scratchpad>/exec-friction/<story>.md` and returns a short
pointer. Mandate:

```
# <story>
## Status
Did it compile? Run? Do what the story asked? Compile attempts: N.
What is verified vs assumed — be honest; unfinished is useful.
## Crate path
## What I built
The wiring in a few lines: which traits/types, in what order.
## Usability friction
Numbered. Each: **I wanted** / **What I tried** / **What happened** /
**How I found the answer** (rustdoc page, example, doc section — or "read
src/") / **Suggested fix**.
## Suspected defects
Numbered. Each: **Doc says** (quote + location) / **Actual** / **Repro**
(test name in my crate) / **Severity** (panic, wrong value, silent accept).
## What went well
```

Name the friction categories or you will get compiler errors back and nothing
else: couldn't find the type/trait, couldn't find which `use` brings a method
into scope, generic bounds or type annotations fighting you, a value with no
obvious constructor, boilerplate you'd want a helper for, a name that misled
you, a doc example that did not compile, behaviour you had to discover by
running. Plus one probe per story (above), and for every agent: **"What did you
need that only an example showed you, and not the rustdoc?"** — that is the
list of docs to promote.

## 4. Review — verify, do not read

Everything in `/dogfood` step 3 holds. Specific to this run:

- **Run every repro.** `cargo test` in the agent's crate. A "defect" whose
  test passes, or fails for a reason unrelated to its claim, is dropped. One
  that holds is re-written as a minimal test against the layer before it
  reaches the summary — the agent's repro usually carries its own bugs.
- **Check the claimed doc line exists and says what was quoted.** Haiku
  paraphrases; a misquote turns intended behaviour into a "bug".
- **"Couldn't find X" — check whether X is findable.** Search the rustdoc
  for it. If it is on the module page, the friction is real but the fix is
  probably discoverability (a link, a re-export, a line in
  `execution-layer.md`), not a missing API.
- **Read the code they wrote.** A program that "works" by bypassing the OMS,
  re-implementing a fold by hand, or ignoring a `Result` is a finding: the
  intended path was harder than the wrong one.

## 5. Synthesise

Two tables — usability and defects — deduplicated, each item with: one-line
problem, stories affected, compile-attempt cost where it shows, concrete fix,
files touched, rough size. **Several stories hitting the same wall is the
signal.** Sort defects by severity first, usability by (stories × cheapness).

Say which fixes are **docs** (rustdoc, `docs/execution-layer.md`, an example
README), which are **API** (a re-export, a constructor, a renamed item — and
whether it is breaking; see "Does a dependency change need a version bump?" in
`CLAUDE.md` for the semver framing), and which are **behaviour**. Name what you
discarded as harness artifact or Haiku's own confusion, so the discard is
visible.

Include the compile-attempt counts per story. They are noisy at n=1, but a
story that took 30 attempts against a median of 6 is pointing somewhere.

## 6. Put it to the user

Summary, then `AskUserQuestion` (multi-select) for which items to implement.
Do not implement before they choose. Defects with a verified repro should be
offered as "fix" *or* "file an issue" — they are real either way.

## 7. Clean up

The scratch crates are disposable. Carry any repro the user wants kept into
`crates/wingfoil/tests/` (as a proper regression test, feature-gated with
`#![cfg(feature = "execution")]`), then `rm -rf <scratchpad>/userland` and
`scripts/disk.sh light`.

## What the first run (2026-10-24) taught

1. **The compile-attempt count was low everywhere (median 1.5)** because
   agents copied call shapes from the examples. That is a finding in itself —
   the examples carry what the rustdoc does not — but it makes the count a
   poor usability metric unless agents record whether they read an example
   first (now a prompt rule).
2. **The defects came from the ordinary stories, not the quality agent.**
   The failure-paths story found no contradiction of a doc line. `burst!`
   not compiling downstream, `Pacing::deferred` counting ratio holds, and
   the re-place-after-fill livelock (issue #1064) all came from agents
   simply trying to build something. Keep the quality story, but read every
   report for defects.
3. **The venue story is the one that pays.** It was the only one to find the
   venue contract undocumented, and the only one to hit the livelock. Never
   drop it from a narrowed run.
4. **"X is undocumented" was wrong twice**: `Desired`'s fields and
   `ReconcileWiring::reading`'s return type are both on their rustdoc
   pages. Grep the built HTML before believing one.
5. **The copied `Cargo.lock` worked**: each scratch crate rebuilt only
   `wingfoil` against the shared cache. One agent read `src/` against the
   rules, and logged it as friction, as asked.

## Feed lessons back into this skill

If a run shows a story that produced no signal, a read-rule agents ignored, a
disk mitigation that failed, or a phrasing that got agents to over-claim — fold
it back here in the same session, and add a "What run N taught" section as
`/dogfood` has.
