# Cutover plan — replacing the legacy tree with Wingfoil

Wingfoil is being built to replace the legacy `wingfoil` tree wholesale.
This document holds the two goals that govern that cutover and the current
status. The phase-by-phase roadmap, the capability matrix, and the gates live
in [`port-plan.md`](port-plan.md).

## Goals

### 1. A strict superset of legacy wingfoil — including examples

Before cutover, everything the legacy tree offers must exist here: every
node/operator, every adapter, every run mode and execution pattern, the
examples, benchmarks, language bindings and docs. Where wingfoil deliberately
deviates (e.g. by-design `compiled()` restrictions), the deviation is
documented in the capability matrix in [`port-plan.md`](port-plan.md) — never
left implicit. Anything legacy does that wingfoil cannot do (or has not explicitly
ruled out) is a cutover blocker.

### 2. Ready to swap out the legacy tree wholesale

Wingfoil *is* the repo root — `README`, `LICENSE`, `CONTRIBUTING`,
`docs/`, and the crates under `crates/` — and the legacy tree sits under
`legacy/`. So the cutover is a deletion, not a re-organisation: `rm -rf
legacy/` plus the crate rename in 1.2, with nothing left to move. Until then,
the legacy crates keep shipping untouched and serve as the permanent parity
oracle for the port.

## The dependency direction — legacy depends on wingfoil, never the reverse

**Nothing under `crates/` may depend on the `wingfoil` crate.** The shared
runtime core lives in `wingfoil`, and the legacy crate depends on
`wingfoil` and re-exports it.

This is the invariant that makes goal 2 achievable. The cutover *deletes* the
legacy crates; anything under `crates/` that still pointed at them would have to
be disentangled first, turning a directory promotion back into a
re-organisation. Pointing the edge the other way means the swap is a delete.

The shared core is `crates/wingfoil/src/runtime/`:

| Module | Contents |
|---|---|
| `runtime::time` | `NanoTime` — engine time |
| `runtime::run` | `RunMode`, `RunFor` — the run bounds |
| `runtime::time_queue` | `TimeQueue` — the scheduled-callback queue |
| `runtime::kernel` | `Kernel`, `KernelWaker`, `ReadyReceiver`, `waker_channel` |
| `runtime::burst` | `Burst<T>` and the `burst!` macro |
| `runtime::latency` | `Latency`, `Stage`, `HasLatency`, `Traced`, `StageStats`, `LatencyStats`, `latency_stages!` |

`wingfoil` re-exports every one of these at its historical path
(`wingfoil::NanoTime`, `wingfoil::codegen::Kernel`, `wingfoil::Traced`, …), so
the legacy public API is unchanged and both engines use **one** set of types
rather than structurally-identical twins — a `Traced` payload or a `RunFor`
crosses the engine boundary without conversion.

Two consequences worth knowing:

- **`latency_stages!` moved to `wingfoil-derive`.** It is part of the
  shared latency data layer, so leaving it in `wingfoil-derive` would have kept
  a wingfoil → legacy edge. `wingfoil-derive` now holds only `#[node]`, which dies
  with the legacy tree (see `port-plan.md` Phase 7).
- **The one remaining edge back to `wingfoil` is a dev-dependency**, for the
  parity tests (`tests/engine_semantics.rs`) and the legacy-vs-wingfoil comparison
  benches (`benches/tiers.rs`). Cargo permits the cycle precisely because it is
  dev-only: the *library* graph never leaves the wingfoil tree.
 That edge is the parity oracle, and it goes away with the legacy tree.

At cutover, then, the legacy tree does not need unpicking. The shared core is
already on the wingfoil-side; what gets deleted is the `MutableNode` wiring path,
`wingfoil/src/nodes/`, `wingfoil-derive`, `wingfoil-python`, and the legacy
examples and benches.

## Status

Phases 0–6 are complete: the node catalog, all 15 adapters, the engine
execution model, the infrastructure, and the Python binding surface. The
shared runtime core has moved to `wingfoil` and the dependency edge is
inverted, so nothing under `crates/` points at the legacy crates.

**Every prerequisite below has now landed**, including the two rulings that
were owed code (2.2, 2.3), all four docs rows (§4), the release plumbing (§5.3–
5.6) and the first of the two swap steps — the crate and module rename (1.2),
which went in over a workspace that legacy had already left (5.0).

What is left is the second swap step: deleting `legacy/` and the scaffolding
that existed only to let the two engines coexist (1.3, 5.2, the 4.3 deletions),
then the verification gates (§6) and the `next` → `main` merge. That sequence
is [`cutover-runbook.md`](cutover-runbook.md).

## The swap is sequenced in two steps, not one

**Decided 2026-08-03.** The rename and the deletion are separated: rename the
new engine's crates to `wingfoil` first, with the legacy tree still on disk but **out of the cargo
workspace**, and delete `legacy/` in a later, purely subtractive step.

That splits the risk. The rename (1.2) is the wide, conflict-prone change and
it now lands on its own; the deletion is then a `git rm` plus workflow and
publish cleanup, with no source edits attached. It also means the legacy tree
survives as a runnable parity oracle *past* the rename, rather than
disappearing in the same commit that makes every `use wingfoil::` stale.

Two consequences follow, and both are load-bearing:

- **Legacy leaves the workspace now** (§5.0). This is the enabler, not just a
  build-time saving: two workspace members cannot both be named `wingfoil`, so
  1.2 cannot land while `legacy/wingfoil` is a member. Excluding legacy also
  drops `legacy/wingfoil-python`'s 13-feature roll-up out of the root feature
  unification, which is what makes a plain `cargo build --workspace` compile
  most of the `full` tree today.
- **The parity dev-dependency needs a rename alias at 1.2.** Once
  the new crate *is* `wingfoil`, its dev-dependency on the legacy crate
  collides with its own package name and must be aliased —
  `legacy_wingfoil = { package = "wingfoil", path = "../../legacy/wingfoil" }`
  — with `tests/engine_semantics.rs` and `benches/tiers.rs` updated to the
  alias. The alias dies with the legacy tree.

Publishing is unaffected in between: the legacy crates stay on crates.io at
their current version, and the renamed crate publishes over the same name at
the 5.6 major bump. What must not happen is the legacy publish jobs running
after 1.2 — they retire with the rename, not with the deletion (§5.3).

## The deletion itself has a runbook

Everything below is done bar the deletion. The step-by-step for that —
what goes, in what order, what is and is not recoverable, and the two
consequences that reach outside the repo — is
[`cutover-runbook.md`](cutover-runbook.md).

## Prerequisite work before cutover starts

What had to be true before the swap could begin, grouped by kind. "Blocking"
meant the swap cannot ship without it; "gating" meant it needs an explicit
decision, which may be to accept a deviation.

**This section is now a record rather than a queue** — every row is either ✅
landed/ruled or ⏸️ deliberately sequenced with the deletion (1.3, 5.2, the 4.3
deletions), which is [`cutover-runbook.md`](cutover-runbook.md)'s subject.

Completed rows are removed as they land. **Row IDs are stable** — they are
referenced from commits, PRs and `port-plan.md` — so gaps in the numbering
are expected and do not mean a row is missing.

### 1. Hard code blockers

Each of these stops the legacy tree from being deletable, or stops wingfoil from
being installable under the legacy name.

| # | Item | Why it blocks | Size |
|:--:|---|---|:--:|
| 1.2 | ✅ **Crate + module rename — landed.** `wingfoil-next` → `wingfoil`, `wingfoil-next-macros` → **`wingfoil-derive`**, taking over legacy's published name (decided 2026-08-03) so the cutover **orphans nothing on crates.io**. Of the four published crates — `wingfoil`, `wingfoil-python`, `wingfoil-wire-types`, `wingfoil-derive` — all four now continue at 9.0.0 rather than three continuing and one stopping dead at 8.0.0. The trade accepted knowingly: `wingfoil-derive` 9.0.0 shares no API with 8.0.0, since its only macro was `#[node]`, which dies with the legacy engine; the major bump is what signals that. It holds `nitro!`, `#[op]` and `latency_stages!` — the last of which genuinely used to live in `wingfoil-derive`, so there is real lineage, not just a reused label, `wingfoil-next-python` → `wingfoil-python`, and the Python module `wingfoil_next` → `wingfoil`. Directories renamed to match. **Two consequences that only appear once both trees carry the name:** each cross-workspace edge needs a `package =` alias (legacy keeps the key `wingfoil-next` so its whole source tree is untouched; wingfoil's dev-dep on legacy becomes `legacy_wingfoil`), and **`-p wingfoil` is now ambiguous** while legacy is on disk — every wingfoil-side invocation moved to `--manifest-path crates/wingfoil/Cargo.toml`, which is stable across version bumps where `-p wingfoil@0.1.0` would not be. | Done. | L |
| 1.3 | ⏸️ **Delete the `wingfoil-derive` crate — deferred to the legacy deletion.** It now holds only `#[node]`, it already sits under `legacy/`, and §5.0 takes it out of the workspace, so it stops mattering to the rename. It goes with `rm -rf legacy/` in the second step, not before. | Not a blocker for 1.2. Nothing under `crates/` depends on it. | S |
| 1.4 | ✅ **Ruled 2026-08-03: no compatibility facade.** The `MutableNode` wiring path retires with the legacy tree at the deletion step; nothing re-exports it under the new name. Rust downstreams break at the major version bump, and [`migration.md`](../migration.md) is the answer — the same call the Python binding already made ("a replacement engine with its own binding, not a compatibility facade over the old one"), and keeping the two languages consistent matters more than softening one of them. A facade would also have to be *maintained* across the very refactors the cutover exists to enable. | Was: decides whether Rust downstreams break at the version bump. They do, deliberately. | M |

### 2. Rulings owed — no code, but they gate the swap

The deviation register's standing instruction: **every remaining 🔴 and 🟡
needs an explicit accept/fix ruling at cutover.**

**All of them are now ruled** (2026-08-03) — the section is kept for the audit
trail rather than as a to-do list. 2.2 and 2.3 were ruled *close it* and have
since landed; the rest are accepts, each with the reasoning that justified it
rather than a bare tick.

| # | Item | Class | Decision needed | Source |
|:--:|---|:--:|---|---|
| 2.1 | ✅ **`Graph::export` (GML topology dump) — ruled 2026-08-03: accept the drop; since *superseded*.** The separately-scoped introspection surface it was deferred to has landed (`src/introspect.rs`): `snapshot()` → text / Mermaid / DOT / JSON / GML, with active and passive edges distinguished. The legacy capability is now a strict subset of what ships, so this is no longer a removal at all. | ⚪ | **Closed.** The migration guide's entry (4.2) now reads as a rename with a mapping table rather than a removal. | register C6; `docs/planning/introspection-plan.md` |
| 2.2 | **Latency ops are fluent/interpreted-only** — `stamp`/`stamp_precise`/`latency_report` have no `nitro!`/`compiled()`/`nested()` form. | ⚪ | **Ruled 2026-08-03: close the gap, don't ratify.** Needs per-op type-argument syntax in `nitro!` — a stamp's stage is a compile-time *type* parameter (`stamp::<quote_latency::produce>()`) and the macro forwards values, which is the whole difficulty. **Own PR.** | register C7 |
| 2.3 | **zmq cross-language interop not ported** — the `bincode` envelope is wingfoil-local, not wire-compatible with a legacy/Python peer. Its stated deferral was "with the Python bindings (Phase 6)", which is now done. | ⚪ | **Ruled 2026-08-03: close the gap.** The deferral has expired and this is a legacy-parity capability, not superset work. **Own PR.** | register C2 |
| 2.4 | ✅ **Ruled 2026-08-03: accept — not a blocker.** Verified against legacy source: it exposes `kafka_sub` and `fluvio_sub` only, both live tails, with **no bounded reader of any kind**. Wingfoil ships those *plus* `kafka_source`/`fluvio_source`, whose historical half errors at wiring naming the unimplemented reader. So wingfoil's surface is a strict superset and the missing half is **new capability legacy never had** — a post-cutover enhancement, not a parity gap. | 🟡 | **Ruled.** | register B2 |
| 2.5 | ✅ **Ruled 2026-08-03: accept as legacy-parity.** Legacy drives its async adapters through `block_on` on the graph thread in exactly the same way, so the constraint is inherited, not introduced. It is also inherent rather than incidental: an owned runtime does not change it, since the runtime's workers are separate threads either way. Documented per-adapter and in the architecture doc. | 🟡 | **Ruled.** | register A5a |
| 2.6 | ✅ **Ruled 2026-08-03: accept.** Values and tick times match legacy; the two residual artifacts are benign and legacy shares the first of them (its `graph_node` delay case desynchronises likewise). Bound `spawn_map` runs by **duration**, not a raw cycle count — noted where it bites. | 🟢🟡 | **Ruled.** | register B6 |
| 2.7 | ✅ **Ruled 2026-08-03: accept all three by design.** They share one reason, which is what makes the ruling easy: **every one is a property of the compiled tier, and legacy has no compiled tier at all**, so none can be a regression against it — each is a limit on capability wingfoil adds. 🟡³ compiled realtime is timer-driven with no external wake (deferred with compiled-path IO ingestion, C4/2.8). 🟡¹⁰ island dynamic-graph is partial — the interpreted surface is full and landed. 🟡¹³ compiled sparse gating is per-node `if` checks rather than region gating, and the `sparse`/`sparse_wide` benchmarks measure those checks cheap enough that compiled still beats the dirty-list on a ~97%-quiet graph, which *lowers* the expected payoff of closing it. | 🟡 | **Ruled.** | port-plan.md capability matrix |
| 2.8 | ✅ **Ruled 2026-08-03: both stay post-v1.** Same reasoning as 2.7 — legacy has no compiled tier, so neither is a parity gap. C3 (multi-output islands) is deferred with the arena, which shares the slot-representation coupling; interpreted multi-output is unaffected and already works (`Builder::demux`). C4 (compiled-path IO ingestion) is a deliberate exclusion: I/O stays at the interpreted boundary with compiled islands inside. | ⚪ | **Ruled.** | register C3/C4 |

### 3. Superset gaps — Goal 1 names examples, benchmarks, bindings and docs

| # | Item | Size |
|:--:|---|:--:|
| 3.7 | ✅ **The statistics Python binding — landed.** `crates/wingfoil-python/src/statistics.rs` binds `Window` / `Weighting` / `EwmaSpan` over `mean`/`variance`/`std`/`sum`/`min`/`max`/`median`/`ewma` as a **dispatcher** onto the engine's `StatisticsOps` — legacy's two orthogonal knobs resolved onto the engine's one-method-per-combination surface, matched exhaustively so a new engine combination is a compile error rather than a silent gap. No engine file touched. All 37 legacy tests ported to `crates/wingfoil-python/tests/test_statistics.py`. | M |
| 3.8 | ✅ **Multi-stream `build_dataframe` in wingfoil-python — landed.** `wingfoil.build_dataframe({name: stream})` outer-joins several already-run streams on engine time, built in Rust beside the single-stream `dataframe()` rather than as a Python helper. Columns may be held as frames (`dataframe()`) or as `(time, value)` tuples (`collect()`, legacy's shape). All 4 legacy tests ported to `crates/wingfoil-python/tests/test_pandas.py`; the legacy `to_dataframe` tests have no counterpart by design (wingfoil builds the frame in the engine) — noted in `docs/migration.rst`. | S |

Row **3.9** (sweep the legacy tree for drift since each phase was ticked) has
run; the findings are below, and it adds no rows.

#### 3.9 — the legacy-drift sweep: ran, no new gaps

**Ran 2026-08-02 against `next` @ `754514c`. Result: 3.7 was the only open
case, so §3 gained no rows — and 3.7 has since landed, closing the section.**

**Window.** `docs/planning/port-plan.md` was created at `6eb7940` (2026-07-19), so no ✅
in it can predate that. The sweep therefore covers the whole life of the file:
every legacy-side commit from `dd94c7b` (2026-07-16, the last legacy commit the
port branch started from) to HEAD. That is a superset of every individual
phase's tick window, which is the point — it cannot miss a phase by getting its
date wrong. The ticks that fall inside it, for reference: spikes 0.1/0.2 at
`50bec39`/`7877be9` (07-19), 0.3/0.4 at `7115136`/`706a3af` (07-20), the Phase 2
catalog at `b774731` (07-25), dynamism at `12592d6` (07-25), `graph_node` at
`5e8299d` (07-26), the last adapter (iceoryx2) at `529085d` (07-30), Phase 4.5
at `9fc68e0` (08-01), Phase 5 at `51c8260` (08-01), and Phase 6's "Surface
build-out" at `5f99b04` (08-02).

**Two git caveats, both of which defeat the obvious query.**

1. The tree inversion `754514c` (#655) moved legacy from `wingfoil/`,
   `wingfoil-python/`, `wingfoil-derive/` to `legacy/*`, so `git log --
   legacy/` reaches back only as far as that commit. Use the pre-inversion
   paths, or `--follow`.
2. **`next` was rebased onto `main`.** Every commit from `13ba842` to `5f99b04`
   carries a committer date of 2026-08-02 09:02–09:03 UTC against author dates
   spanning 07-19 to 08-02. Ancestry order is therefore *not* chronological
   order: `git log da919bb..HEAD` returns the entire port, because the whole
   port line was re-parented onto `main`'s head. Select legacy commits by
   author date, not by ancestry.

Also note the clone in a fresh sandbox is shallow — `git fetch --unshallow`
first, or the window silently truncates to the last 50 commits.

**Method.** Enumerated every commit touching the legacy paths in the window
(`git log --full-history -- wingfoil/ wingfoil-python/ wingfoil-derive/
legacy/`), classified each as legacy-originated or wingfoil-originated, and read
the diff of every legacy-originated one. Then cross-checked structurally, so
that anything the date filter somehow missed would still surface as a missing
file: legacy's 24 examples, 6 benches, 43 `src/nodes/` modules, 18
`src/adapters/` entries and 21 `wingfoil-python/tests/` files against wingfoil's
equivalents.

**What actually landed on the legacy line during the port — four commits,
`dd94c7b..da919bb`:**

| Commit | Date | What | Verdict |
|---|---|---|---|
| `d561c52` (#589) | 2026-07-27 | `wingfoil/src/nodes/drop_small_change.rs`, `PyStream.drop_small_change`, 3 pytest cases | **Drift** — landed 2 days after the Phase 2 catalog ✅ (`b774731`). **Already closed** by `991bfa7`: the op across all three engines, the fluent method, the Python binding and all three tests (`crates/wingfoil/src/ops.rs`, `src/fluent.rs`, `tests/catalog.rs`, `tests/op_completeness.rs`, `crates/wingfoil-python/src/graph.rs`, `tests/test_interop.py`) |
| `f5b6915` (#590), `23fa547` (#591) | 2026-07-27 | `wingfoil-js/package.json` + `pnpm-lock.yaml` npm-audit patches | **Not a parity target** — `wingfoil-js` is now `js/` and survives the cutover |
| `da919bb` (#611) | 2026-08-01 | `wingfoil-python/src/py_statistics.rs` (319 lines), 8 statistics methods on `PyStream`, `Window`/`Weighting`/`EwmaSpan` exports, `tests/test_statistics.py` (249 lines) | **Drift** — this was row **3.7**, whose description matched the diff exactly; **since closed** (the full statistics binding, all 37 tests) (the `py_augurs.rs` hunk in the same commit is a pure refactor, hoisting `as_floats` into `py_stream.rs`) |

**Everything else that touched legacy files in the window was wingfoil-originated,
and none of it is a parity target.** `13ba842` / `6465d3d` / `bfbe24f` /
`9bd66cc` grew the `wingfoil::codegen` retrofit and `09359a9` (#480) deleted it
outright; `5e8299d` and `af7284e` extended the surviving `codegen.rs` `Kernel`
(since moved to `runtime::kernel`); `4601716` refactored `wingfoil/src/
latency.rs` to share `record_stage_deltas` / `format_latency_report` with the
Python surface; `8b5d6b9` (#604) fixed the kafka `consume_messages` helper in
**both** trees in one commit; `0c49838` and `754514c` are the dependency and
directory inversions.

**Why 3.7 was missed — the mechanism, stated precisely.** 3.7's note says
legacy grew `py_statistics.rs` *after* the "Surface build-out ✅" was written.
The timestamps say the reverse — `da919bb` landed 2026-08-01 08:27 UTC, the ✅
at `5f99b04` was authored 2026-08-02 08:57 UTC — but the real mechanism is the
same failure and slightly worse. `next` had not yet been rebased onto `main`
when that ✅ was written (the rebase is stamped 09:02 UTC, five minutes later),
so the tree it was written against carried a legacy snapshot no newer than
`dd94c7b` (2026-07-16). **Every ✅ from Phase 0 through Phase 6's surface
build-out was written blind to both #589 and #611.** That is the stale-base
hazard the Order section already warns about, in its documentation form: a ✅ is
a claim about whatever legacy snapshot the branch happens to be carrying, not
about `main`. `drop_small_change` is the proof — it was noticed at `991bfa7`,
authored 09:39 UTC, 36 minutes *after* the rebase first brought the node into
the tree, and by nothing more systematic than tripping over it.

**Forward risk: low, and cheap to re-check.** `main` has had no functional
legacy change since `da919bb` (2026-08-01) and the legacy tree is frozen in
practice, so the expected yield of a re-run is zero. But the sweep is now one
command rather than an archaeology exercise — everything is on one branch,
under one path, past the inversion:

```bash
git log --format='%h %ad %s' --date=short 754514c..HEAD -- legacy/
```

Anything that returns is a parity target that landed after this sweep. That is
gate **6.5**, run immediately before the swap.

### 4. Docs the cutover owes

**All four landed in one PR** (#675, as decided 2026-08-03) — they were one
editorial pass over the same story, and 4.2 and 4.4 overlapped enough that
splitting them would have meant writing the architecture prose twice.

| # | Item | Size |
|:--:|---|:--:|
| 4.1 | ✅ **Crate-level docs rewritten.** `crates/wingfoil/src/lib.rs` used to open *"**Design prototype**: what wingfoil's core abstractions look like if designed from scratch…"* and spend its first 40 lines on a post-mortem of the abandoned `codegen` retrofit. It now opens on what the library *is*, with a runnable graph in the first screenful. | M |
| 4.2 | ✅ **Migration guide `#[node]` → `Op` — written** ([`migration.md`](../migration.md)). Carries the Rust facade decision (1.4 — there is no facade) and 2.1's ruling: `Graph::export` is named as the one removed public API. The Python half stays where it was and is referenced, not duplicated — `crates/wingfoil-python/docs/migration.rst`. | M |
| 4.3 | 🟢 **Root `CLAUDE.md` edits done; the `legacy/` copies retire at deletion.** The promotion itself was already done — wingfoil's copies *are* the root copies and the originals moved to `legacy/`. What this PR owed was the root `CLAUDE.md`: the legacy branching section survives (legacy is still on disk and still cut from `main`) and now says legacy is out of the workspace and how to build it. Deleting `legacy/README` / `CONTRIBUTING` / `CLAUDE.md` and stripping that section rides with the deletion — runbook step 1 and step 5. | S |
| 4.4 | ✅ **Architecture / orientation doc written** — [`wingfoil-architecture.md`](../wingfoil-architecture.md) (was #507). Deferred until the refactor settled; it had, and this is what a new contributor reads first. | M |

### 5. Repo, CI and release plumbing — Goal 2's "directory promotion"

5.0 landed first, and 1.2 with it; the release-facing rows followed. **Only 5.2
is left, and it is deliberately sequenced with the deletion** — see its row.

| # | Item | Size |
|:--:|---|:--:|
| 5.0 | **Take `legacy/` out of the workspace — do this now.** Drop the three `legacy/*` members and `exclude` the directory. All three legacy crates inherit `rust-version` / `lints` / `workspace.dependencies`, so they need a nested workspace root at `legacy/Cargo.toml` carrying those tables; an excluded package is not a member and cannot inherit. The `wingfoil` → `wingfoil` dev-dependency (`tests/engine_semantics.rs`, `benches/tiers.rs`) keeps working — a path dependency may cross a workspace boundary — so the parity oracle is unaffected. Legacy-side CI moves to `--manifest-path legacy/Cargo.toml`, and `cargo lint` / `lint-all` stop covering legacy by construction. | M |
| 5.1 | ✅ **Workspace `Cargo.toml` — folded into 5.0.** The four wingfoil crates are already at `crates/*`, so no repointing is left. `crates/wingfoil-wire-types` stays (wingfoil's web adapter depends on it); `crates/wingfoil-wasm` stays excluded. | S |
| 5.2 | ✅ **Workflow set collapsed early.** Done ahead of the deletion, as part of removing "next" as a name for this engine: the `*-next-integration.yml` workflows took the plain names and the ~14 legacy twins moved to `legacy-*`, along with `python-test.yml` → `legacy-python-test.yml` and `rust-test.yml`'s `test` / `test-next` jobs → `test-legacy` / `test`. The cost accepted knowingly: CI check names churn **twice** (now, and again when the `legacy-*` files are deleted), so any required-status-check configuration needs updating both times. `legacy-augurs-integration.yml` still has no wingfoil twin by design (wingfoil's augurs tests run in `rust-test.yml` under `--all-features`). The legacy web workflow was **deleted rather than renamed**: its only jobs built `crates/wingfoil-wasm` and `js/`, which survive the cutover, so they moved into `web-integration.yml` — where the wire contract they share with the server side already lives. | M |
| 5.3 | ✅ **`crates-publish.yml` rewritten.** Legacy's publish steps are *gone*, not disabled — both trees build a crate named `wingfoil`, so publishing both would race for one registry name. Three blockers surfaced only by running `cargo publish --dry-run`: every crate still carried `publish = false`; the intra-workspace path deps had no `version`; and there was no license/authors/homepage metadata, with `wingfoil`'s crates.io description still reading "Design prototype". | M |
| 5.4 | ✅ **`pypi-publish.yml` repointed, and #367 ruled: the wheel ships aeron and iceoryx2.** `aeron` was missing from the `all-adapters` roll-up entirely. Because the matrix builds macOS and Windows too — where iceoryx2 (POSIX-only) and aeron (C library) would *stop the wheel building* rather than enrich it — the Linux wheel is built from `all-adapters` and the other platforms keep the portable pyproject set. | S |
| 5.5 | ✅ **`.readthedocs.yaml` repointed** — all three keys together. | S |
| 5.6 | ✅ **Bumped to 9.0.0** — over legacy's 8.x line, which matters now the renamed crate owns `wingfoil` on crates.io. Legacy stays at 8.0.0 (frozen; its `wingfoil-wire-types` pin still moves, or it stops resolving). `bump.yml` now reads its base from `crates/wingfoil` — left on legacy it would have pegged every future release below the shipped version. The `rust-test`/`all-tests`/`rust-fmt` branch filters stay `[main, next]` until the next→main swap. | S |

### 6. Verification gates — run immediately before the swap

| # | Gate |
|:--:|---|
| 6.1 | `cargo fmt --all -- --check`, `cargo lint`, `cargo lint-all` green on the promoted tree. Read the exit codes directly — piping into `tail`/`head` masks them. |
| 6.2 | `cargo test --manifest-path crates/wingfoil/Cargo.toml --all-features` and the `wingfoil-python` pytest suite green. |
| 6.3 | Every adapter integration workflow green on the cutover branch — they gate the service-backed adapters the unit suites cannot. |
| 6.4 | ✅ **Read 2026-08-03 — the gate passes.** Captured here because it cannot be re-run later: the legacy bar disappears with the tree, so this is the only record that will survive. See the table below. |
| 6.5 | Re-run the legacy-drift sweep: `git log --format='%h %ad %s' --date=short 754514c..HEAD -- legacy/`. Empty output means every ✅ in `port-plan.md` still describes the legacy tree as it *is*, not as it was; anything it returns is a parity target that landed after the [3.9 sweep](#39--the-legacy-drift-sweep-ran-no-new-gaps) and needs a row in §3 before the swap. Seconds to run, and the sweep it replaces cost an afternoon. |

### Open issues to route

**The re-labelling has happened.** When this section was written only **#602**
(aeron: fragment assembly, sub ergonomics, publisher back-pressure) carried the
`next` label and the survivors were still under `classic`. All **26** open
issues now carry `next`, so the routing question is closed and what is left is
per-issue triage, not a sweep. Named here because the runbook's step 8 acts on
them: **#367** (wheel excludes aeron/iceoryx2) is **resolved by 5.4** and should
be closed rather than kept; **#450** (no manylinux/aarch64/sdist wheels, trusted
publishing), **#452** (Dependabot alerts, wasm lockfile), **#449 / #451 / #359**
(CI blind spots, workflow dedup, stale actions), **#461** (supply-chain
hardening), **#457** (wingfoil-js) and **#437** (web historical streaming is
lossy — confirm whether wingfoil's web adapter already fixes it) all survive the
swap and stay open.

### Order

Five PRs, in this order (agreed 2026-08-03). **All five have landed:**

| | PR | Depends on | Landed |
|:--:|---|---|---|
| 1 | **5.0** — legacy out of the workspace | — | ✅ #671 |
| 2 | **2.3** — zmq cross-language interop | — | ✅ #672 |
| 3 | **2.2** — latency ops in `nitro!` / `compiled()` / `nested()` | — | ✅ #674 |
| 4 | **§4** — all four docs rows, one PR | 2.1 (ruled), 1.4 (ruled) | ✅ #675 |
| 5 | **1.2** — the crate + module rename | 5.0 | ✅ #679 |

2, 3 and 4 were mutually independent and independent of 1; they only had to be
in before 1.2, because **1.2 touches every `use wingfoil::` in the tree**
and conflicts with anything open across it. It was landed with the tree quiet.
The rest of section 5 followed it (§5.3–5.6, #680), leaving `rm -rf legacy/` —
with 1.3, the 4.3 deletions and the legacy workflow/publish retirement — as the
separate second step, now written up as [`cutover-runbook.md`](cutover-runbook.md).

**Section 3 is closed**: 3.9 ran and added nothing, and 3.7 and 3.8 have both
landed. Its standing replacement is gate 6.5, which re-checks the sweep
immediately before the swap.

**Every ruling owed by §2 has been given** (2026-08-03) — 1.4 was the last one
anything waited on, since 4.2 could not be written without knowing whether the
legacy facade API survived. It does not.

One sequencing hazard, learned the hard way: a branch cut before an
invariant lands will happily reintroduce what that invariant removed, and CI
on a stale base will not catch it. Rebase onto `next` before merging anything
that has been open across a structural change.

### Gate 6.4 — the legacy-vs-wingfoil reading, captured before deletion

Run 2026-08-03 on the merged tree, `cargo bench --bench tiers`. Median times.

| group | legacy | wingfoil interpreted | gain | wingfoil compiled | vs legacy |
|---|---:|---:|---:|---:|---:|
| dense_chain | 8.74 ms | 8.43 ms | 3.6% | 302 µs | 28.9× |
| fanout | 20.36 ms | 15.78 ms | 22.5% | 439 µs | 46.3× |
| fan_in_16 | 4.91 ms | 2.99 ms | 39.0% | 283 µs | 17.4× |
| fan_in_64 | 14.08 ms | 8.97 ms | 36.3% | 380 µs | 37.1× |
| fan_in_256 | 50.18 ms | 36.14 ms | 28.0% | 3.03 ms | 16.6× |
| accumulate | 2.51 ms | 1.57 ms | 37.6% | 486 µs | 5.2× |
| sparse | 3.18 ms | 2.49 ms | 21.9% | 426 µs | 7.5× |
| sparse_wide | 3.54 ms | 2.69 ms | 24.0% | 547 µs | 6.5× |

**The gate — `wingfoil-interpreted ≥ legacy-interpreted` — passes in all eight
groups**, by 3.6% to 39%. Compiled is 5–46× faster than legacy throughout.

Read these as a **pass/fail on the gate, not as publication figures**: they come
from a shared sandbox with criterion's measurement window shortened to 3s. Every
margin except `dense_chain`'s 3.6% is far outside that noise; `dense_chain` is
the one to re-read on a quiet machine if the exact number matters. What the gate
asks — that the port cost nothing against the engine it replaces — is answered.
