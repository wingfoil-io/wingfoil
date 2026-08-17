Dogfood this repository with a team of small-model agents and harvest the
friction they hit. Scope: `$ARGUMENTS` names what the team should build (e.g.
`3 ops and 2 examples`, or a specific list). Leave it blank and you choose the
tasks yourself from the gaps described in step 1.

This is a **measurement** skill. The code the agents write is the vehicle, not
the product — the product is a deduplicated, actionable list of everything that
made contributing to this repo harder than it needed to be, put to the user as
a menu of fixes to choose from.

## Why a deliberately weak model

Run the team on **Haiku** by default. This is counter-intuitive and it is the
whole point: a strong model silently infers past ambiguity in the docs and
skills, so it finds nothing. A weak model hits every under-specified step
head-on and reports it. Haiku failures are a map of where the documentation is
thin.

The corollary is that **broken code is an expected, acceptable outcome**. Tell
the agents so explicitly, or they will over-claim success. A friction log that
says "did not compile, 3 errors remaining, here is where I got stuck" is worth
more than a clean-looking diff.

Offer the user the choice up front if they have not already made it —
Haiku for friction signal, Sonnet/Opus if they actually want landable code —
and say plainly which output each buys.

## 1. Choose the tasks

Aim for **6–8 agents**, one task each, spread deliberately across surfaces so
the friction is not all from one recipe. A good spread:

- **3–4 ops** via `/new-op` — one simple stateful op, one predicate/closure op,
  one time-scheduling op (`Activation::SCHEDULES`), one statistics-family op.
- **1 op with Python bindings** — the `#[pyop]` / `pyop_fn!` step is where
  `/new-op` is thinnest; give it its own agent and say so in the prompt.
- **2–3 `core/` examples** — these exercise a completely different rule set
  (the directory + README + explicit Cargo target + two README index links,
  checked by `scripts/check-example-docs.sh`).
- **1 adapter binding** via `/bind-adapter` — pick an adapter that is ported in
  Rust but genuinely unbound in Python. Compare
  `crates/wingfoil/src/adapters/` against
  `crates/wingfoil-python/src/adapters/` to find one; do not invent a gap that
  is already filled.

Pick tasks that are real gaps, not busywork. Before spawning, grep to confirm
the op/example/binding does not already exist — an agent that discovers its
task is already done produces no friction data.

## 2. Spawn the team

All agents in **one message** so they run concurrently, each with
`isolation: "worktree"` so they cannot conflict, and `model: "haiku"`.

### Protect the disk — this is the failure mode that kills the run

Eight worktrees each building independently will exhaust the sandbox's
writable allowance (see "Disk space" in `CLAUDE.md`). Two mitigations, both
mandatory:

1. **Pre-warm the shared dependency cache** before spawning:
   `CARGO_INCREMENTAL=0 cargo check -p wingfoil --features statistics`, run in
   the background while you write the prompts.
2. **Every agent prompt must set** `export
   CARGO_TARGET_DIR=/home/user/wingfoil/target`. Registry dependencies are
   keyed by package id and features, not by workspace path, so all agents reuse
   the one ~700-crate dep cache and only recompile the workspace crate itself.
   Cargo locks the directory, so checks serialise — tell the agents that
   `Blocking waiting for file lock` is expected and to wait it out.

### Rules every agent prompt must carry

- **Allowed**: `cargo check -p <crate> [--features ...]`,
  `cargo test -p wingfoil --lib <filter>`, `cargo run -p wingfoil --example
  <name>`, `cargo fmt`, `scripts/check-example-docs.sh`.
- **Forbidden**: `--all-targets`, `--all-features`, `cargo build --release`,
  `cargo bench`, `maturin`, `pytest`. The first four exhaust the disk; the last
  two are unavailable. Where a skill step calls for maturin or pytest, the agent
  should **skip the run but still write the tests**, and record the unverified
  step in its friction log.
- **Do not `git commit`** — `.cargo-husky`'s pre-commit hook runs a full
  `clippy --workspace --all-targets`, which is exactly the build the disk rules
  forbid. Agents leave changes uncommitted in their worktree and report the
  path.
- **Read `CLAUDE.md` first**, then invoke the matching skill via the Skill tool
  (`Skill(skill="new-op")` etc.) rather than reimplementing its steps.

### The friction log

Have each agent **write its log to a file** under the scratchpad
(`<scratchpad>/friction/<name>.md`) as well as returning a short summary. The
file is the durable artifact you review; the return text is only a pointer.
Mandate this structure:

```
# <task name>
## Status
Did it compile? Did tests pass? What was verified vs assumed? What could not
be verified and why? Be honest — a failed build is useful data.
## Worktree path
## What I changed
Files touched, one line each.
## Friction log
Numbered. Each entry: **Expected** / **What happened** / **Where** (file:line
or skill step) / **Suggested fix** (concrete).
## What went well
Which parts of the docs/skills genuinely worked.
```

Prompt for the categories you want covered, or you will only get compiler
errors back: missing or wrong docs, contradictory instructions, under-specified
skill steps, steps the agent had to guess at, unclear errors, slow or awkward
tooling, and **boilerplate mechanical enough to automate**. Add one
task-specific probe per agent — e.g. ask the examples agents to count how many
separate places a new example must be registered and whether anything catches a
missed one; ask the bindings agents whether an external user has everything
exported and documented.

## 3. Review

Read every friction log file. Then, independently, **read the actual diffs** —
`git -C <worktree> diff` — because agent self-reports are unreliable in both
directions: they claim success they did not verify, and they omit friction they
worked around without noticing. Cross-check status claims against the code.

## 4. Synthesise

Deduplicate across agents. **Friction reported independently by several agents
is the signal**; a single agent's confusion is often just that model being
weak, so weight by how many agents hit it and whether the diff corroborates it.

Turn each surviving item into an actionable entry with: the problem in one
line, how many agents hit it, the concrete fix, which files it touches, and a
rough size. Sort by (agents affected × cheapness of fix). Separate genuine repo
defects from artifacts of the harness (disk limits, forbidden commands,
Haiku's own comprehension failures) — the latter are not fixes to offer.

## 5. Put it to the user

Present the summary, then use `AskUserQuestion` (multi-select) to ask which
items to implement. Do not start implementing before they choose. Note which
fixes belong in a skill file versus in code or CI, since per `CLAUDE.md` the
three skills are living documents and folding lessons back into them is part of
"done".

## 6. Clean up

The worktrees hold uncommitted work. Once the user has chosen, carry the
selected changes onto the working branch and remove the rest
(`git worktree remove`). Run `scripts/disk.sh light` afterwards — eight
worktrees leave a lot behind.

## What the first run (2026-08-16) taught

All five are about **not trusting the reports**, and they cost most of the
review time on that run:

1. **Self-reports are wrong in both directions, so verify every one.** One
   agent reported a hard blocker ("`#[op(fluent)]` doesn't generate the macro")
   that was simply false — the code compiled; its op logic was broken instead,
   which it never found. Another reported clean success while introducing the
   first `panic!` into a crate that had zero. A third reported "all tests pass"
   for an op that silently deviated from its spec and encoded the deviation as
   a passing test. Run the build yourself, run the tests yourself, and read the
   diff. Budget for this — it is the bulk of the work.
2. **Confident root-cause attributions from a weak model are the most
   expensive output.** "The proc macro is broken" would have sent a maintainer
   into `wingfoil-derive` for nothing. Treat any *diagnosis* in a friction log
   as a symptom report; re-derive the cause before it reaches the summary.
3. **Check reported gaps against the docs before believing them.** Several
   "X is missing / undocumented" items were wrong: `Burst`, `Activation`, `Ctx`
   and `Tick` are all in the prelude, and `_common` is documented twice in
   `/bind-adapter`. The underlying friction was real but the agent's
   explanation of it was not — and the *correct* diagnosis usually implies a
   different fix.
4. **A shared `CARGO_TARGET_DIR` manufactures its own friction.** Three of four
   op agents reported needing `cargo clean` for "proc macro regeneration". That
   is fingerprint thrash from the shared directory, not a repo defect. Keep the
   sharing — it is what makes eight concurrent agents fit on the disk — but
   discount that entire class of report, and say so in the summary rather than
   letting it look like a finding.
5. **Prompt-level build prohibitions are not self-enforcing.** An agent ran
   `cargo lint` (a full `--all-targets` clippy) despite an explicit ban. Size
   the disk headroom to survive a few such violations rather than assuming
   compliance, and check `df` between waves.

## Feed lessons back into this skill

If a run surfaces something this recipe does not capture — a task shape that
produced no useful signal, a disk mitigation that failed, a prompt phrasing
that got agents to over-claim — fold it back into this file in the same
session.
