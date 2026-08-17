Dogfood this repository with a team of small-model agents and harvest the
friction they hit. `$ARGUMENTS`, if given, narrows or overrides the fixed team
in step 1; blank runs the standard eight.

This is a **measurement** skill. The code the agents write is the vehicle, not
the product. The product is a deduplicated, actionable list of everything that
made contributing here harder than it needed to be, put to the user as a menu
of fixes to choose from.

## The model is Haiku. Do not ask, do not offer alternatives.

Counter-intuitive and load-bearing: a strong model silently infers past
ambiguity in the docs and so finds nothing — its silence is not evidence. A
weak model hits every under-specified step head-on. Haiku failures are a map of
where the documentation is thin, which is the entire output.

**Broken code is therefore an expected, acceptable outcome.** Say so in every
agent prompt or they will over-claim success. "Did not compile, 3 errors
remaining, here is where I got stuck" is worth more than a clean-looking diff.

## 1. The team — eight agents, fixed composition

Spread deliberately so the friction is not all from one recipe:

- **6 examples**, each on a *different* uncovered part of the surface —
  see "Choosing the six" below. Examples exercise a rule set nothing else
  touches: directory + README + explicit Cargo target + two index links,
  enforced by `scripts/check-example-docs.sh`, with the README's sample output
  required to be real.
- **1 op with Python bindings**, via `/new-op`. The `#[pyop]` / `pyop_fn!` step
  is the thinnest part of that skill, so say in the prompt that binding
  friction is what you most want reported.
- **1 new adapter with Python bindings**, via `/new-adapter` *then*
  `/bind-adapter`. This is the heaviest task in the set and the one most likely
  to come back unfinished — that is fine and it is the point: it is the only
  task that crosses two skills, and the seam between them is exactly where the
  instructions have never been walked end to end.

Every task must be a **real gap**. Before spawning, confirm it: grep `ops.rs`
for the op, `examples/` for the example, and compare
`crates/wingfoil/src/adapters/` against `crates/wingfoil-python/src/adapters/`
for genuinely unbound adapters. An agent that discovers its task is already
done produces no friction data and wastes a slot.

### Choosing the six — derive it, don't guess

The examples *are* the coverage map, so read it out of the tree rather than
picking from memory. Three commands:

```bash
# 1. What is already covered — declared targets and their features.
#    (Skip the leading [[bench]] blocks; examples start at hello_graph.)
awk '/^\[\[example\]\]/{if(n)print n" ["rf"]";n="";rf="-";next}
     /^\[/{if(n)print n" ["rf"]";n="";rf="-"}
     /^name[[:space:]]*=/{split($0,a,"\"");n=a[2]}
     /^required-features/{rf=$0;sub(/^[^=]*= */,"",rf);gsub(/[]["]/,"",rf)}
     END{if(n)print n" ["rf"]"}' crates/wingfoil/Cargo.toml

# 2. The whole feature surface, to diff against it.
sed -n '/^\[features\]/,/^\[dependencies\]/p' crates/wingfoil/Cargo.toml

# 3. Adapters that exist in the engine.
ls crates/wingfoil/src/adapters/
```

Then diff: any feature or adapter with no example target, and any engine
concept you cannot find with `grep -rl '<api>' crates/wingfoil/examples/`, is a
candidate. **Confirm each candidate with that grep before assigning it** — the
cost of getting this wrong is a wasted agent, and it is easy to get wrong.

Snapshot as of 2026-08-16, useful as a seed but **re-derive rather than
trusting it**:

| Uncovered | Why it is a gap |
|---|---|
| **latency capture** (`latency::Stamp`, `StampEach`, `LatencyReport`) | only reachable inside `showcase/latency` and `trading_e2e`, both multi-process and iceoryx2-gated — there is no small example teaching the stamping API on its own |
| **`cache` adapter** | no example anywhere in the tree |
| **`market` adapter** | mentioned once in the `ws` README, never demonstrated |
| **`Signal` facade** (`signal.rs`) | appears only incidentally in `adapters/iceoryx2` |
| **`window` / `buffer`** | no example calls either — the bounded look-back `CLAUDE.md` recommends instead of `accumulate()` |
| **`dhat-heap`, `bench`/`bencher.rs`, `instrument-*`** | profiling and granular instrumentation features with no worked example |
| **`ws-tls` / `web-tls`** | only exercised through `trading_e2e` |

Already covered — do **not** spend a slot on these: `demux` and `demux_map`
have examples of their own, and `fan` plus the `nested()` island tier are both
demonstrated in `dual_mode`.

Spread the six across *kinds* as well as features: at least one needing no
feature flag, one adapter-group example, and one that is purely an engine
concept. An example that needs a live external service is a poor choice — the
agent cannot run it, and an unrunnable example cannot have real sample output,
which is the rule most worth testing.

## 2. Spawn the team

All eight in **one message** so they run concurrently, each with
`isolation: "worktree"` and `model: "haiku"`.

### Protect the disk — the failure mode that kills the run

Eight worktrees building independently will exhaust a sandbox's writable
allowance (see "Disk space" in `CLAUDE.md`). Both mitigations are mandatory:

1. **Pre-warm the shared dependency cache** before spawning, in the background
   while you write the prompts:
   `CARGO_INCREMENTAL=0 cargo check -p wingfoil --features statistics`.
2. **Every agent prompt sets** `export
   CARGO_TARGET_DIR=/home/user/wingfoil/target`. Registry dependencies are
   keyed by package id and features, not workspace path, so all agents share
   the one ~700-crate cache and recompile only the workspace crate. Cargo locks
   the directory, so checks serialise — tell agents `Blocking waiting for file
   lock` is expected and to wait.

Check `df` between waves. Prompt-level bans are not self-enforcing (see the
lessons below), so size headroom to survive a few violations.

### Rules every agent prompt must carry

- **Allowed**: `cargo check -p <crate> [--features …]`, `cargo test -p wingfoil
  --lib <filter>`, `cargo run -p wingfoil --example <name>`, `cargo fmt`,
  `scripts/check-example-docs.sh`, `scripts/check-python-bindings.sh`.
- **Forbidden**: `--all-targets`, `--all-features`, `cargo build --release`,
  `cargo bench`, `maturin`, `pytest`. The first four exhaust the disk; the last
  two are unavailable. Where a skill step calls for maturin or pytest, the agent
  **skips the run but still writes the tests** and records the unverified step.
- **Do not `git commit`** — `.cargo-husky`'s pre-commit hook runs a full
  `clippy --workspace --all-targets`, precisely the build the disk rules avoid.
  Agents leave work uncommitted and report the worktree path.
- **Read `CLAUDE.md` first**, then invoke the matching skill via the Skill tool
  (`Skill(skill="new-op")`) rather than reimplementing its steps.

### The friction log

Each agent **writes its log to a file** under the scratchpad
(`<scratchpad>/friction/<name>.md`) and returns only a short summary. The file
is the durable artifact; the return text is a pointer. Mandate this shape:

```
# <task name>
## Status
Compiled? Tests passed? What was verified vs assumed? What could not be
verified, and why? A failed build is useful data — be honest.
## Worktree path
## What I changed
Files touched, one line each.
## Friction log
Numbered. Each: **Expected** / **What happened** / **Where** (file:line or
skill step) / **Suggested fix** (concrete).
## What went well
Which parts of the docs/skills genuinely worked.
```

Name the categories you want or you will get only compiler errors back: wrong
or missing docs, contradictory instructions, under-specified steps, guesses,
unclear errors, awkward tooling, and **boilerplate mechanical enough to
automate**. Add one task-specific probe per agent — ask the examples agents how
many places an example must be registered and whether anything catches a miss;
ask the bindings agents whether an external user has everything exported and
documented.

## 3. Review — verify, do not read

Read every log, then **independently check every claim**: run the build, run
the tests, read `git -C <worktree> diff`. Self-reports are unreliable in both
directions — they claim success they never verified and omit friction they
worked around without noticing. This is the bulk of the work; budget for it.

## 4. Synthesise

Deduplicate. **Friction hit independently by several agents is the signal**; a
lone agent's confusion is often just the model being weak. Weight by agents
affected and whether the diff corroborates it.

Each surviving item gets: the problem in one line, how many agents hit it, the
concrete fix, files touched, rough size. Sort by (agents affected × cheapness).
Separate genuine repo defects from harness artifacts — disk limits, forbidden
commands, Haiku's own comprehension failures are not fixes to offer, but say
out loud that you discarded them so the discard is visible.

## 5. Put it to the user

Present the summary, then `AskUserQuestion` (multi-select) for which items to
implement. Do not implement before they choose. Flag which fixes belong in a
skill file versus code or CI — per `CLAUDE.md` the skills are living documents,
and folding lessons back is part of "done".

## 6. Clean up

Worktrees hold uncommitted work. Once the user has chosen, carry the selected
changes onto the working branch, `git worktree remove` the rest, and run
`scripts/disk.sh light`.

## What the first run (2026-08-16) taught

All five are about **not trusting the reports**, and they cost most of that
run's review time:

1. **Self-reports are wrong in both directions.** One agent reported a hard
   blocker ("`#[op(fluent)]` doesn't generate the macro") that was false — the
   code compiled, and its op logic was broken instead, which it never found.
   Another reported clean success while introducing the first `panic!` into a
   crate that had zero. A third reported "all tests pass" for an op that had
   silently deviated from its spec and encoded the deviation as a passing test.
2. **A weak model's confident root-cause attribution is its most expensive
   output.** "The proc macro is broken" would have sent a maintainer into
   `wingfoil-derive` for nothing. Treat every *diagnosis* as a symptom report
   and re-derive the cause before it reaches the summary.
3. **Check reported gaps against the docs before believing them.** Several
   "X is undocumented" items were wrong: `Burst`, `Activation`, `Ctx` and
   `Tick` are all in the prelude, and `_common` is documented twice in
   `/bind-adapter`. The friction was real; the explanation was not — and the
   correct diagnosis usually implies a different fix.
4. **The shared `CARGO_TARGET_DIR` manufactures its own friction.** Three of
   four op agents reported needing `cargo clean` for "proc macro regeneration" —
   fingerprint thrash, not a repo defect. Keep the sharing, discount the
   reports, and say so in the summary rather than letting it look like a
   finding.
5. **Prompt-level build prohibitions are not self-enforcing.** An agent ran
   `cargo lint` despite an explicit ban.

## Feed lessons back into this skill

If a run surfaces something this recipe misses — a task shape that produced no
signal, a disk mitigation that failed, a phrasing that got agents to over-claim
— fold it back into this file in the same session.
