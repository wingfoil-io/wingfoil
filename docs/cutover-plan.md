# Cutover plan — replacing the legacy tree with Wingfoil Next

Wingfoil Next is being built to replace the legacy `wingfoil` tree wholesale.
This document holds the two goals that govern that cutover and the current
status. The phase-by-phase roadmap, the capability matrix, and the gates live
in [`port-plan.md`](port-plan.md).

## Goals

### 1. A strict superset of legacy wingfoil — including examples

Before cutover, everything the legacy tree offers must exist here: every
node/operator, every adapter, every run mode and execution pattern, the
examples, benchmarks, language bindings and docs. Where next deliberately
deviates (e.g. by-design `compiled()` restrictions), the deviation is
documented in the capability matrix in [`port-plan.md`](port-plan.md) — never
left implicit. Anything legacy does that next cannot do (or has not explicitly
ruled out) is a cutover blocker.

### 2. Ready to swap out the legacy tree wholesale

Wingfoil Next *is* the repo root — `README`, `LICENSE`, `CONTRIBUTING`,
`docs/`, and the crates under `crates/` — and the legacy tree sits under
`legacy/`. So the cutover is a deletion, not a re-organisation: `rm -rf
legacy/` plus the crate rename in 1.2, with nothing left to move. Until then,
the legacy crates keep shipping untouched and serve as the permanent parity
oracle for the port.

## The dependency direction — legacy depends on next, never the reverse

**Nothing under `crates/` may depend on the `wingfoil` crate.** The shared
runtime core lives in `wingfoil-next`, and the legacy crate depends on
`wingfoil-next` and re-exports it.

This is the invariant that makes goal 2 achievable. The cutover *deletes* the
legacy crates; anything under `crates/` that still pointed at them would have to
be disentangled first, turning a directory promotion back into a
re-organisation. Pointing the edge the other way means the swap is a delete.

The shared core is `crates/wingfoil-next/src/runtime/`:

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

- **`latency_stages!` moved to `wingfoil-next-macros`.** It is part of the
  shared latency data layer, so leaving it in `wingfoil-derive` would have kept
  a next → legacy edge. `wingfoil-derive` now holds only `#[node]`, which dies
  with the legacy tree (see `port-plan.md` Phase 7).
- **The one remaining edge back to `wingfoil` is a dev-dependency**, for the
  parity tests (`tests/engine_semantics.rs`) and the legacy-vs-next comparison
  benches (`benches/tiers.rs`). Cargo permits the cycle precisely because it is
  dev-only: the *library* graph runs `wingfoil` → `wingfoil-next` and nothing
  more. That edge is the parity oracle, and it goes away with the legacy tree.

At cutover, then, the legacy tree does not need unpicking. The shared core is
already on the next side; what gets deleted is the `MutableNode` wiring path,
`wingfoil/src/nodes/`, `wingfoil-derive`, `wingfoil-python`, and the legacy
examples and benches.

## Status

Phases 0–6 are complete: the node catalog, all 15 adapters, the engine
execution model, the infrastructure, and the Python binding surface. The
shared runtime core has moved to `wingfoil-next` and the dependency edge is
inverted, so nothing under `crates/` points at the legacy crates.

What is left is the Phase 7 cutover itself, plus the prerequisites below.

## The swap is sequenced in two steps, not one

**Decided 2026-08-03.** The rename and the deletion are separated: rename next
to `wingfoil` first, with the legacy tree still on disk but **out of the cargo
workspace**, and delete `legacy/` in a later, purely subtractive step.

That splits the risk. The rename (1.2) is the wide, conflict-prone change and
it now lands on its own; the deletion is then a `git rm` plus workflow and
publish cleanup, with no source edits attached. It also means the legacy tree
survives as a runnable parity oracle *past* the rename, rather than
disappearing in the same commit that makes every `use wingfoil_next::` stale.

Two consequences follow, and both are load-bearing:

- **Legacy leaves the workspace now** (§5.0). This is the enabler, not just a
  build-time saving: two workspace members cannot both be named `wingfoil`, so
  1.2 cannot land while `legacy/wingfoil` is a member. Excluding legacy also
  drops `legacy/wingfoil-python`'s 13-feature roll-up out of the root feature
  unification, which is what makes a plain `cargo build --workspace` compile
  most of the `full` tree today.
- **The parity dev-dependency needs a rename alias at 1.2.** Once
  `wingfoil-next` *is* `wingfoil`, its dev-dependency on the legacy crate
  collides with its own package name and must be aliased —
  `legacy_wingfoil = { package = "wingfoil", path = "../../legacy/wingfoil" }`
  — with `tests/engine_semantics.rs` and `benches/tiers.rs` updated to the
  alias. The alias dies with the legacy tree.

Publishing is unaffected in between: the legacy crates stay on crates.io at
their current version, and the renamed crate publishes over the same name at
the 5.6 major bump. What must not happen is the legacy publish jobs running
after 1.2 — they retire with the rename, not with the deletion (§5.3).

## Prerequisite work before cutover starts

Everything still outstanding before the directory promotion can begin,
grouped by kind. "Blocking" means the swap cannot ship without it; "gating"
means it needs an explicit decision, which may be to accept a deviation.

Completed rows are removed as they land. **Row IDs are stable** — they are
referenced from commits, PRs and `port-plan.md` — so gaps in the numbering
are expected and do not mean a row is missing.

### 1. Hard code blockers

Each of these stops the legacy tree from being deletable, or stops next from
being installable under the legacy name.

| # | Item | Why it blocks | Size |
|:--:|---|---|:--:|
| 1.2 | **Crate + module rename.** `wingfoil-next` → `wingfoil`, `wingfoil-next-macros` → the derive crate, `wingfoil-next-python` → `wingfoil-python`, and the Python module `wingfoil_next` → `wingfoil`. | Cutover is a *name* takeover, not just a directory move. Touches every `use wingfoil_next::` in the tree, every doc link, every example, every workflow, and both publish jobs. **Head of the critical path**, and it conflicts with anything else in flight — land it with the tree quiet. | L |
| 1.3 | ⏸️ **Delete the `wingfoil-derive` crate — deferred to the legacy deletion.** It now holds only `#[node]`, it already sits under `legacy/`, and §5.0 takes it out of the workspace, so it stops mattering to the rename. It goes with `rm -rf legacy/` in the second step, not before. | Not a blocker for 1.2. Nothing under `crates/` depends on it. | S |
| 1.4 | **Retire the legacy engine internals** (`MutableNode` wiring path) and rule on whether the legacy facade API survives the swap. | Decides whether Rust downstreams break at the version bump. | M |

### 2. Rulings owed — no code, but they gate the swap

The deviation register's standing instruction: **every remaining 🔴 and 🟡
needs an explicit accept/fix ruling at cutover.** These are the open ones.

| # | Item | Class | Decision needed | Source |
|:--:|---|:--:|---|---|
| 2.1 | ✅ **`Graph::export` (GML topology dump) — ruled 2026-08-03: accept the drop.** Next ships no `export`; a better introspection/visualisation story is scoped separately, and nothing in the engine blocks reintroducing one (`Builder` holds the full topology plus debug labels). | ⚪ | **Ruled.** Residual work is one line in the migration guide (4.2) naming it as the single removed public API. | register C6 |
| 2.2 | **Latency ops are fluent/interpreted-only** — `stamp`/`stamp_precise`/`latency_report` have no `nitro!`/`compiled()`/`nested()` form. | ⚪ | **Ruled 2026-08-03: close the gap, don't ratify.** Needs per-op type-argument syntax in `nitro!` — a stamp's stage is a compile-time *type* parameter (`stamp::<quote_latency::produce>()`) and the macro forwards values, which is the whole difficulty. **Own PR.** | register C7 |
| 2.3 | **zmq cross-language interop not ported** — the `bincode` envelope is next-local, not wire-compatible with a legacy/Python peer. Its stated deferral was "with the Python bindings (Phase 6)", which is now done. | ⚪ | **Ruled 2026-08-03: close the gap.** The deferral has expired and this is a legacy-parity capability, not superset work. **Own PR.** | register C2 |
| 2.4 | **Live sources reject `RunMode::HistoricalFrom` at wiring.** Split ruling already agreed; the residual is that `kafka`/`fluvio` `_source` carry a live half only. | 🟡 | Confirm the bounded kafka/fluvio readers are *superset* work, not parity (legacy never offered them), so they do not block. | register B2 |
| 2.5 | **`block_on` sinks/readers panic if the graph is driven from an async context.** | 🟡 | Accept as legacy-parity and inherent to `block_on`-on-the-graph-thread. | register A5a |
| 2.6 | **`spawn_map` historical lock-step artifacts** — a filtering/delaying sub-graph desynchronises; bound runs by duration, not raw cycle count. | 🟢🟡 | Accept (legacy's `graph_node` delay case fails likewise). | register B6 |
| 2.7 | **Three capability-matrix 🟡s**: compiled realtime is timer-driven with no external wake (🟡³); island dynamic-graph is partial (🟡¹⁰); compiled sparse gating is per-node `if` checks, not region gating (🟡¹³). | 🟡 | Three accept-by-design rulings, or work items. | port-plan.md capability matrix |
| 2.8 | **C3 multi-output islands** and **C4 compiled-path IO ingestion** (busy-poll + bursts). | ⚪ | Confirm both stay post-v1; neither is a legacy-parity gap, since legacy has no compiled tier. | register C3/C4 |

### 3. Superset gaps — Goal 1 names examples, benchmarks, bindings and docs

| # | Item | Size |
|:--:|---|:--:|
| 3.7 | ✅ **The statistics Python binding — landed.** `crates/wingfoil-next-python/src/statistics.rs` binds `Window` / `Weighting` / `EwmaSpan` over `mean`/`variance`/`std`/`sum`/`min`/`max`/`median`/`ewma` as a **dispatcher** onto the engine's `StatisticsOps` — legacy's two orthogonal knobs resolved onto the engine's one-method-per-combination surface, matched exhaustively so a new engine combination is a compile error rather than a silent gap. No engine file touched. All 37 legacy tests ported to `crates/wingfoil-next-python/tests/test_statistics.py`. | M |
| 3.8 | ✅ **Multi-stream `build_dataframe` in next-python — landed.** `wingfoil_next.build_dataframe({name: stream})` outer-joins several already-run streams on engine time, built in Rust beside the single-stream `dataframe()` rather than as a Python helper. Columns may be held as frames (`dataframe()`) or as `(time, value)` tuples (`collect()`, legacy's shape). All 4 legacy tests ported to `crates/wingfoil-next-python/tests/test_pandas.py`; the legacy `to_dataframe` tests have no counterpart by design (next builds the frame in the engine) — noted in `docs/migration.rst`. | S |

Row **3.9** (sweep the legacy tree for drift since each phase was ticked) has
run; the findings are below, and it adds no rows.

#### 3.9 — the legacy-drift sweep: ran, no new gaps

**Ran 2026-08-02 against `next` @ `754514c`. Result: 3.7 was the only open
case, so §3 gained no rows — and 3.7 has since landed, closing the section.**

**Window.** `docs/port-plan.md` was created at `6eb7940` (2026-07-19), so no ✅
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
legacy/`), classified each as legacy-originated or next-originated, and read
the diff of every legacy-originated one. Then cross-checked structurally, so
that anything the date filter somehow missed would still surface as a missing
file: legacy's 24 examples, 6 benches, 43 `src/nodes/` modules, 18
`src/adapters/` entries and 21 `wingfoil-python/tests/` files against next's
equivalents.

**What actually landed on the legacy line during the port — four commits,
`dd94c7b..da919bb`:**

| Commit | Date | What | Verdict |
|---|---|---|---|
| `d561c52` (#589) | 2026-07-27 | `wingfoil/src/nodes/drop_small_change.rs`, `PyStream.drop_small_change`, 3 pytest cases | **Drift** — landed 2 days after the Phase 2 catalog ✅ (`b774731`). **Already closed** by `991bfa7`: the op across all three engines, the fluent method, the Python binding and all three tests (`crates/wingfoil-next/src/ops.rs`, `src/fluent.rs`, `tests/catalog.rs`, `tests/op_completeness.rs`, `crates/wingfoil-next-python/src/graph.rs`, `tests/test_interop.py`) |
| `f5b6915` (#590), `23fa547` (#591) | 2026-07-27 | `wingfoil-js/package.json` + `pnpm-lock.yaml` npm-audit patches | **Not a parity target** — `wingfoil-js` is now `js/` and survives the cutover |
| `da919bb` (#611) | 2026-08-01 | `wingfoil-python/src/py_statistics.rs` (319 lines), 8 statistics methods on `PyStream`, `Window`/`Weighting`/`EwmaSpan` exports, `tests/test_statistics.py` (249 lines) | **Drift** — this was row **3.7**, whose description matched the diff exactly; **since closed** (the full statistics binding, all 37 tests) (the `py_augurs.rs` hunk in the same commit is a pure refactor, hoisting `as_floats` into `py_stream.rs`) |

**Everything else that touched legacy files in the window was next-originated,
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

**All four land in one PR** (decided 2026-08-03) — they are one editorial pass
over the same story, and 4.2 and 4.4 overlap enough that splitting them would
mean writing the architecture prose twice.

| # | Item | Size |
|:--:|---|:--:|
| 4.1 | **Rewrite the crate-level docs.** `crates/wingfoil-next/src/lib.rs` still opens *"**Design prototype**: what wingfoil's core abstractions look like if designed from scratch…"* and spends its first 40 lines on a post-mortem of the abandoned `codegen` retrofit. That becomes the crate's front page the moment it is `wingfoil`. | M |
| 4.2 | **Migration guide `#[node]` → `Op`.** Does not exist. Carries the Rust facade decision (1.4) and 2.1's ruling — `Graph::export` is the one removed public API. The Python half is largely written — `wingfoil-next-python/docs/migration.rst` — and can be referenced rather than duplicated. | M |
| 4.3 | **Retire the `legacy/` copies of `README` / `CONTRIBUTING` / `CLAUDE.md`.** The promotion itself is done — next's copies *are* the root copies and the originals moved to `legacy/`. Deleting them rides with the legacy deletion, so what this PR owes is the root `CLAUDE.md` edits: the legacy branching section survives (legacy is still on disk and still cut from `main`) but must now say legacy is out of the workspace and how to build it. The strip happens at deletion. | S |
| 4.4 | **Architecture / orientation doc** (`docs/wingfoil-next-architecture.md`, was #507). Deferred until the refactor settled; it has, and cutover is exactly when a new contributor needs it. | M |

### 5. Repo, CI and release plumbing — Goal 2's "directory promotion"

5.0 lands **first, before 1.2** — it is what makes the rename possible. The
rest is blocked on 1.2, which fixes the names it all refers to.

| # | Item | Size |
|:--:|---|:--:|
| 5.0 | **Take `legacy/` out of the workspace — do this now.** Drop the three `legacy/*` members and `exclude` the directory. All three legacy crates inherit `rust-version` / `lints` / `workspace.dependencies`, so they need a nested workspace root at `legacy/Cargo.toml` carrying those tables; an excluded package is not a member and cannot inherit. The `wingfoil-next` → `wingfoil` dev-dependency (`tests/engine_semantics.rs`, `benches/tiers.rs`) keeps working — a path dependency may cross a workspace boundary — so the parity oracle is unaffected. Legacy-side CI moves to `--manifest-path legacy/Cargo.toml`, and `cargo lint` / `lint-all` stop covering legacy by construction. | M |
| 5.1 | ✅ **Workspace `Cargo.toml` — folded into 5.0.** The four next crates are already at `crates/*`, so no repointing is left. `crates/wingfoil-wire-types` stays (next's web adapter depends on it); `crates/wingfoil-wasm` stays excluded. | S |
| 5.2 | **Collapse the workflow set.** 14 of 43 workflows carry `next` and lose the suffix; the legacy-side twins retire (at the deletion — 5.0 only repoints them at the nested manifest). The three `latency-e2e.*` workflows are already repointed at `crates/wingfoil-next/examples/latency_e2e/`, so they need nothing at cutover. Note `augurs-integration.yml` has no next twin **by design** — next's augurs tests run in `rust-test.yml`'s `test-next` job under `--all-features`. | M |
| 5.3 | **`crates-publish.yml` rewrite.** It publishes by directory in dependency order with crates.io index waits between; the crate set, the order and the paths all change. | M |
| 5.4 | **`pypi-publish.yml` repoint**, plus a ruling on the wheel's adapter roll-up: aeron is out of both roll-ups (it builds a C library) and iceoryx2 is in `all-adapters` but out of the wheel (Linux/POSIX-only). That is open issue **#367** — decide whether cutover inherits it or fixes it. | S |
| 5.5 | **`.readthedocs.yaml` repoint.** Three keys change together: `python.install[0].path`, `python.install[1].requirements`, `sphinx.configuration` — all currently on `legacy/wingfoil-python/`, all moving to `crates/wingfoil-next-python/`. The RTD job builds the extension from source at the full wheel feature set, so it compiles librdkafka and libzmq and needs `protobuf-compiler` (already in `apt_packages`). | S |
| 5.6 | **Major version bump**, and the `rust-test` / `all-tests` / `rust-fmt` path and branch filters (currently `[main, next]`). | S |

### 6. Verification gates — run immediately before the swap

| # | Gate |
|:--:|---|
| 6.1 | `cargo fmt --all -- --check`, `cargo lint`, `cargo lint-all` green on the promoted tree. Read the exit codes directly — piping into `tail`/`head` masks them. |
| 6.2 | `cargo test -p wingfoil-next --all-features` and the next-python pytest suite green. |
| 6.3 | Every `*-next-integration` workflow green on the cutover branch — they gate the service-backed adapters the unit suites cannot. |
| 6.4 | `cargo bench --bench tiers` re-read. The `next-interpreted ≥ legacy-interpreted` baseline can only be checked while the legacy bar exists, so this is the last chance. A manual read: wiring benches as a CI gate stays deliberately deferred, criterion wall-clock being too noisy on shared runners. |
| 6.5 | Re-run the legacy-drift sweep: `git log --format='%h %ad %s' --date=short 754514c..HEAD -- legacy/`. Empty output means every ✅ in `port-plan.md` still describes the legacy tree as it *is*, not as it was; anything it returns is a parity target that landed after the [3.9 sweep](#39--the-legacy-drift-sweep-ran-no-new-gaps) and needs a row in §3 before the swap. Seconds to run, and the sweep it replaces cost an afternoon. |

### Open issues to route

Only **#602** (aeron: fragment assembly, sub ergonomics, publisher
back-pressure) carries the `next` label. Most issues under the `classic`
label — still named that on GitHub, and a candidate for renaming with the
rest — die with the legacy tree, but these survive the swap and want
re-labelling rather than closing: **#367** (wheel excludes aeron/iceoryx2 —
see 5.4), **#450**
(no manylinux/aarch64/sdist wheels, trusted publishing), **#452** (Dependabot
alerts, wasm lockfile), **#449 / #451 / #359** (CI blind spots, workflow
dedup, stale actions), **#461** (supply-chain hardening), **#457**
(wingfoil-js), and **#437** (web historical streaming is lossy — confirm
whether next's web adapter already fixes it).

### Order

Five PRs, in this order (agreed 2026-08-03):

| | PR | Depends on |
|:--:|---|---|
| 1 | **5.0** — legacy out of the workspace | — |
| 2 | **2.3** — zmq cross-language interop | — |
| 3 | **2.2** — latency ops in `nitro!` / `compiled()` / `nested()` | — |
| 4 | **§4** — all four docs rows, one PR | 2.1 (ruled), 1.4 |
| 5 | **1.2** — the crate + module rename | 5.0 |

2, 3 and 4 are mutually independent and independent of 1; they only need to be
in before 1.2, because **1.2 touches every `use wingfoil_next::` in the tree**
and conflicts with anything open across it. Land it with the tree quiet. The
rest of section 5 follows it, and `rm -rf legacy/` (with 1.3, the 4.3
deletions and the legacy workflow/publish retirement) is the separate second
step.

**Section 3 is closed**: 3.9 ran and added nothing, and 3.7 and 3.8 have both
landed. Its standing replacement is gate 6.5, which re-checks the sweep
immediately before the swap.

1.4 is the one ruling still owed that anything else waits on — 4.2 cannot be
written without knowing whether the legacy facade API survives.

One sequencing hazard, learned the hard way: a branch cut before an
invariant lands will happily reintroduce what that invariant removed, and CI
on a stale base will not catch it. Rebase onto `next` before merging anything
that has been open across a structural change.
