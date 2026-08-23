Audit the wingfoil I/O adapters against the `/new-adapter` skill and
the strict-superset parity obligation. Scope: `$ARGUMENTS` names a single
adapter (e.g. `redis`) to review just that one; leave it blank to review **all**
wingfoil adapters under `crates/wingfoil/src/adapters/`.

This is a **read-and-report** skill — it changes no adapter code. Its four
deliverables map to the four things a maintainer needs to know before trusting
the wingfoil adapter surface:

1. **Skill health** — is `.claude/commands/new-adapter.md` itself still
   correct, internally consistent, and non-contradictory with
   `CLAUDE.md`, `docs/planning/port-plan.md`, and the code it points at?
2. **Compliance** — does each adapter obey the skill's invariants and step
   requirements?
3. **Lessons to fold back** — did recent adapter work surface a pitfall / gate /
   pattern the skill doesn't yet capture, per its own "Feed lessons back into
   this skill" mandate?
4. **Unjustified legacy deviations** — every place a wingfoil adapter differs from
   its legacy twin that is **not** documented (module-doc `# Deviations from
   legacy` block **and** the register + port-plan matrix).

The output is a written report, not commits. Only touch files if the review
concludes the *skill* or the *deviation docs* should change **and** the user
asked you to apply fixes — otherwise report and stop.

## 0. Orient (do this once, in the parent context)

Read the ground truth before dispatching any per-adapter work:

- `.claude/commands/new-adapter.md` — the skill under review. Read it end
  to end; the audit checklist below is derived from it, but the file is the
  authority. If it has grown rules since this review skill was last touched,
  audit against the **file**, and note the drift in deliverable 1.
- `CLAUDE.md` — the superset objective and the "skills are living
  documents" mandate.
- `docs/planning/deviation-register.md` — the classified list of known
  legacy↔wingfoil deviations (the parity audit cross-checks against this).
- `docs/planning/port-plan.md` — Phase 4 adapter status + the capability matrix +
  "Known parity gaps".
- `docs/decisions/source-lifecycle.md`, `runtime-ownership.md` — the
  open design items the skill references (A1–A5).

Then enumerate the review set:

```bash
# wingfoil adapters (single-file modules and directory modules)
ls crates/wingfoil/src/adapters/
# every documented deviation block currently in the tree
grep -rn "Deviations from legacy" crates/wingfoil/src/adapters/
# the legacy adapter set, from git history (the tree was deleted at cutover)
DEL=$(git log --format=%H --diff-filter=D -1 -- legacy/CLAUDE.md)
git ls-tree -d --name-only "$DEL^" legacy/wingfoil/src/adapters/
```

`$DEL^` is the last commit that still carried the legacy tree; use
`git show "$DEL^:<path>"` to read any file out of it.

Classify each wingfoil adapter up front — the audit differs by kind:

- **Ported** (a legacy twin existed under `legacy/wingfoil/src/adapters/<name>/`)
  → full parity audit applies. That legacy module, read out of `$DEL^`, is the
  **parity oracle**.
- **Wingfoil-only** (no legacy twin — e.g. `lines`) → parity audit is N/A; audit
  only conventions/invariants, and confirm naming/layering stay backport-ready.
- **Shared helper** (`common.rs`, `mod.rs`) → not an adapter; check only that
  the slicer cfg-gate and the `mod.rs` doc index are correct.

Legacy adapters with **no** wingfoil twin yet (today: `aeron`, `fix`, `fluvio`,
`iceoryx2`, `kdb`, `web`) are out of scope for compliance but belong in the
port-plan coverage note (deliverable 4's tail): confirm they're still tracked
as unported in `port-plan.md`, not silently dropped.

## 1. Per-adapter audit (dispatch to subagents, fresh context each)

For anything beyond a single small adapter, run the per-adapter audits as
**parallel subagents** — one per adapter — so each reads its adapter, the
adapter's legacy twin, and the skill with a clean context, and returns a
structured finding list. This mirrors step 15 of `new-adapter` ("self-review
with a fresh context") and keeps the parent context free to synthesise.

Give each subagent this checklist. It is the `new-adapter` invariants and
step requirements turned into pass/fail probes — cite the skill section on every
finding so the report is traceable, and quote the offending code with a
`file:line`.

### A. Invariants (skill "Invariants" section — the load-bearing rules)

- **No locks on the graph path.** No `Mutex`/`RwLock` `.lock()` inside any
  `Op::cycle`/`start`/`stop`/`teardown`, `for_each`/`for_each_mut`/`map`/`try_map`
  closure, or `poll` closure. Locks are allowed **only** in wiring/factory
  functions, background threads, and cross-thread handles. A graph→background
  ad-hoc *whole-value* hand-off must use `arc_swap::ArcSwap`, **not** a lock
  (the prometheus per-slot pattern). Graph-thread-local mutability uses
  `RefCell`/`Rc<RefCell<…>>`. → flag any `.lock()` reachable from a cycle.
- **Historical replay is deterministic.** A channel-replay source stamps every
  record with `send_at` at **non-decreasing** timestamps ≥ run start, turns
  index overflow into a clear error (`u32::try_from`), `close()`s at end, and
  propagates malformed input via `send_error` (never `panic!`, never a silent
  skip). Prefer the `replay_results` primitive over a hand-rolled
  `channel`→`send_at`→`close` loop. Same-instant records ride one `Burst` (don't
  pre-flatten).
- **Time-sliced replay reuses the shared slicer.** An adapter that replays a
  caller-parameterised time range must build on the ported
  `adapters/common.rs` helpers (`WindowFilter`, `compute_validated_time_slices`),
  **not** a hand-rolled window clamp. A *reusing* adapter widens the existing
  `#[cfg(any(feature = …))]` gate on the slicer — it does not duplicate it. The
  `WindowFilter` row-clamp is always-compiled; only the slicer is feature-gated.
- **Live sources are realtime-only.** A live, never-closing source
  (`*_sub`/watch/consumer) **rejects `RunMode::HistoricalFrom` at wiring** with
  an adapter-named error and returns `Result`. Only finite timestamped sources
  run historically. → confirm the reject exists and is stated in the module docs.
- **Fallibility with context.** Wiring-time I/O is in a factory returning
  `anyhow::Result`; a live socket/subscription's connect + thread spawn is
  deferred to `start()` via `source_at_start` (the `zmq_sub` reference), so
  wiring stays pure. Every I/O boundary carries `.context(...)` naming the
  adapter + resource. No `.unwrap()` outside `#[cfg(test)]`/doc examples. A
  closed receiver (`send` returns `false`) exits the producer loop quietly — not
  an error.
- **Credential redaction.** Any connection string / DSN / URL that can embed a
  secret has a `redacted()` method used at **every** `connect()` error site, and
  a no-service test asserts the raw secret never appears in the error. → for
  every networked adapter with credentials (redis, kafka, zmq, postgres, etcd,
  …), confirm both the method and the test exist.
- **Layering / no prelude.** Sources are free fns taking `&GraphBuilder` first;
  sinks are extension traits on `Stream<Burst<T>>`; compute ops are extension
  traits; **nothing** is added to the prelude. Wiring goes through `Stream::wire`
  / `GraphBuilder::source` only. Verb naming matches the skill's table
  (`_sub`/`_pub`, `_read`/`_write`, `replay_*`/`tail_*`, exporter `_gauge`/…).

### B. Structural requirements (skill steps 3–13)

- **Feature gating (step 3).** Deps optional + feature-gated; dep versions pinned
  to the legacy adapter's (or a *documented* forward-roll for a security
  advisory — the otlp opentelemetry-0.32 precedent, D5). `-integration-test`
  feature for service-backed adapters; none for file/pure-compute.
- **Module registration (step 4).** *Both* `mod.rs` edits present: the gated
  `pub mod` (alphabetical) **and** the `//!` doc-index bullet.
- **Module docs (step 6).** `//!` header has the Layering section, documents
  every public item, `# Errors` on fallible factories, and — if ported — a
  `# Deviations from legacy` block.
- **Realtime-only sinks (steps 2, 8).** An exporter/server/push sink guards on
  `ctx.run_mode()` and **no-ops under historical replay**.
- **Status streams (step 8a).** If present, a `*_with_status` tuple factory
  (primary signature unchanged), transition-only emission, post-success recording.
- **Tests (step 10).** Historical determinism (`RunMode::HistoricalFrom(ZERO)`,
  assert values **and** tick times), unique temp paths (pid+counter), the
  connection-refused-first order for service adapters, correct file-level `cfg`.
- **Example (step 11)** registered with `required-features`; **CI (step 12)**
  workflow + hub registration for service adapters; **Python (step 12)**
  `#[pyadapter]` surface + pytest where applicable; **port-plan (step 13)**
  row updated.

### C. Parity (skill "parity obligation" + step 13) — ported adapters only

Diff the wingfoil adapter against `git show "$DEL^:legacy/wingfoil/src/adapters/<name>/…"`:

- Every public capability (function, config knob, mode enum, event/entry type)
  has a wingfoil equivalent **or** an explicitly documented deviation.
- Every legacy unit test is ported as a parity test (identical values **and**
  tick times), or a comment names why not.
- The legacy example is ported.
- Legacy `CLAUDE.md` design decisions are carried into the module docs.
- Error-message compatibility is kept where legacy tests assert on messages.

**Every gap here is a finding unless it is documented in all the places it must
be:** the module-doc `# Deviations from legacy` block, `deviation-register.md`
(with a class 🔴/🟡/🟢/⚪/✅), and the `port-plan.md` matrix. A deviation
documented in code but missing from the register — or vice-versa — is itself a
finding (doc drift).

Each subagent returns, per finding: `{adapter, section-of-skill, severity,
file:line, what, why-it-matters}`, plus a one-line per-adapter verdict
(compliant / minor / needs-work).

## 2. Skill-health review (deliverable 1 — parent context)

Independently of the adapters, read `new-adapter.md` critically:

- **Internal consistency** — do any two rules contradict? (e.g. a step saying
  "connect at wiring" where the invariants now say "defer to `start()`".)
- **Currency vs the code** — every primitive/type/path the skill names
  (`source_at_start`, `replay_results`, `for_each_mut`, `consume_async`,
  `consume_async_bursts`, `ArcSwap` slots, `#[pyadapter]`, the `common.rs`
  slicer, referenced reference files like `zmq.rs`/`csv.rs`/`lines.rs`/`augurs.rs`)
  must still exist and mean what the skill says. Grep for each; a dangling
  reference is a finding.
- **Currency vs the design docs** — items the skill calls "open"/"tracked"
  (defer-to-start A1/A2, runtime-ownership A5) should agree with the current
  state in `deviation-register.md` and the two design docs. The register is
  ahead of the skill in places (e.g. the `produce_async` family and
  `postgres_write` have since deferred to `start()`); where the skill's prose
  ("Not on `source_at_start` yet: …") has fallen behind the register, that is a
  **lesson to fold back** (deliverable 3), not just a note.
- **Consistency with `CLAUDE.md`** — branching (cut from / merge into
  `next`), pre-commit checklist, out-of-prelude rule.

## 3. Lessons-to-fold-back review (deliverable 3 — parent context)

The skill's own "Feed lessons back into this skill" section makes keeping it
current part of "done". Hunt for lessons the tree has learned that the skill
hasn't absorbed yet:

- **Mine recent adapter PRs.** `git log --oneline next -- crates/wingfoil/src/adapters/`
  and the deviation register's "Resolved / ratified" list. Each resolved item
  (B1 `consume_async` flush teardown, B3 `consume_async_bursts`, B2 unified
  `<adapter>_source`, A5 graph-owned runtime, the `produce_async`/`postgres_write`
  defer-to-start) is a candidate rule: does the skill now *tell the next author
  to do it that way*, or does it still describe the pre-fix world?
- **Recurring findings from §1.** If the same compliance miss shows up in two+
  adapters, the skill probably lacks an explicit rule — propose the rule.
- **New CI gates / sandbox caveats** encountered (the `dependency-review` gate,
  the `cargo lint-all` aeron/CMake sandbox caveat) — confirm the skill still
  documents them and that the documented fix still matches CI.

Report each as a concrete proposed edit to `new-adapter.md` (section +
wording), so folding it back is mechanical.

## 4. Synthesise the report

Produce a single markdown report with these sections:

1. **Skill health** — contradictions, stale references, doc-drift; each with the
   fix.
2. **Compliance matrix** — one row per adapter: kind (ported/wingfoil-only/helper),
   verdict, and the count of findings by severity. Then the findings, grouped by
   adapter, most-severe first, each citing the skill section and `file:line`.
3. **Lessons to fold back** — proposed `new-adapter.md` edits, each with the
   triggering evidence (PR / register item / repeated finding).
4. **Unjustified legacy deviations** — deviations found in §1.C that are **not**
   fully documented (module doc **and** register **and** port-plan). A deviation
   present in all three is *justified* and is **not** listed here (mention the
   count of justified deviations for context). Close with the port-plan coverage
   note: are the still-unported legacy adapters (`aeron`/`fix`/`fluvio`/
   `iceoryx2`/`kdb`/`web`) still tracked as gaps, or has one been dropped?

Rank everything by impact: a correctness/safety invariant breach (a lock on the
graph path, a leaked credential, a live source that doesn't reject historical, a
non-deterministic replay) outranks a missing doc bullet. Give a bottom-line
verdict: is the wingfoil adapter surface compliant with its skill and a faithful
superset of the legacy adapters, or not — and the top three things to fix.

## 5. If asked to apply fixes

By default this skill only reports. If the user asks you to *act* on the
findings:

- **Skill / doc edits** (deliverables 1 & 3, and documenting a justified-but-
  undocumented deviation from 4) are safe to apply here — edit
  `new-adapter.md`, `deviation-register.md`, or `port-plan.md`, then run
  nothing heavier than a re-read (these are docs). Follow the branch rules in
  `CLAUDE.md` (this is wingfoil work — the branch is cut from `main`).
- **Adapter code fixes** (a real compliance breach) are *not* this skill's job —
  each belongs on its own branch through `/new-adapter`'s pre-commit
  checklist (fmt + `cargo lint` + `cargo lint-all` + tests). Hand them back as a
  prioritised worklist, don't fold them into the review branch.

Commit doc changes with a clear message and push to the designated branch; open
a PR only if the user asks, with base `main`.
