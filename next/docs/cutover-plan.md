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

The `next/` folder mirrors the legacy repo root — `README`, `LICENSE`,
`CONTRIBUTING`, `docs/`, and the crates under `crates/` — so the eventual
cutover is a directory promotion, not a re-organisation. Until then, the
legacy crates keep shipping untouched and serve as the permanent parity oracle
for the port.

## The dependency direction — legacy depends on next, never the reverse

**Nothing under `next/` may depend on the `wingfoil` crate.** The shared
runtime core lives in `wingfoil-next`, and the legacy crate depends on
`wingfoil-next` and re-exports it.

This is the invariant that makes goal 2 achievable. The cutover *deletes* the
legacy crates; anything under `next/` that still pointed at them would have to
be disentangled first, turning a directory promotion back into a
re-organisation. Pointing the edge the other way means the swap is a delete.

The shared core is `next/crates/wingfoil-next/src/runtime/`:

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
the classic public API is unchanged and both engines use **one** set of types
rather than structurally-identical twins — a `Traced` payload or a `RunFor`
crosses the engine boundary without conversion.

Two consequences worth knowing:

- **`latency_stages!` moved to `wingfoil-next-macros`.** It is part of the
  shared latency data layer, so leaving it in `wingfoil-derive` would have kept
  a next → legacy edge. `wingfoil-derive` now holds only `#[node]`, which dies
  with the legacy tree (see `port-plan.md` Phase 7).
- **The one remaining edge back to `wingfoil` is a dev-dependency**, for the
  parity tests (`tests/engine_semantics.rs`) and the classic-vs-next comparison
  benches (`benches/tiers.rs`). Cargo permits the cycle precisely because it is
  dev-only: the *library* graph runs `wingfoil` → `wingfoil-next` and nothing
  more. That edge is the parity oracle, and it goes away with the legacy tree.

At cutover, then, the legacy tree does not need unpicking. The shared core is
already on the next side; what gets deleted is the `MutableNode` wiring path,
`wingfoil/src/nodes/`, `wingfoil-derive`, `wingfoil-python`, and the legacy
examples and benches.

## Status

Porting is in progress, phase by phase, with the legacy test suite as the
parity oracle — see [`port-plan.md`](port-plan.md) for the live ✅/🟡/⬜ state.
The port can pause at any phase boundary with everything shipped still
correct; the legacy crates remain the production engine until the superset
objective above is met.

Phases 0–5 are complete, Phase 4 (adapters) is complete across all 15
adapters, and Phase 6's Python binding surface is complete. What is left is
the Phase 7 work itself plus the prerequisites below.

## Prerequisite work before cutover starts

Everything that must be settled *before* the directory promotion begins,
grouped by what kind of blocker it is. "Blocking" means the swap cannot ship
without it; "gating" means it needs an explicit decision but may be resolved
by accepting the deviation.

**Provenance.** Every row below was checked against the tree, not taken from
the plan documents — the cited `file:line` is the check. Two things that
survey turned up, both now fixed in place:

- **Capability-matrix footnote ⁴ was stale.** It claimed compiled/island emit
  `start` only, with `stop`/`teardown` waiting on a macro-expressible op that
  needs them ("none do yet"). In fact `#[op]` emits real `_stop`/`_teardown`
  forwarders for every op and both emission targets call them error-safe at
  the loop tail — `finally` is exactly the op that needs it, and
  `tests/compiled_lifecycle_ops.rs` pins it across all three engines. The
  matrix row is now ✅/✅ and the footnote rewritten.
- **1.1's blast radius was understated.** It is not a handful of kernel
  imports: 41 files, every adapter, plus a `pub use wingfoil;` that puts the
  legacy crate in next's public API and in every `nitro!` expansion.

Re-run the checks after any phase lands; the rows are only as current as the
commit they were verified against.

**One row's check was too shallow, and doing the work exposed it.** Row 3.6
was verified by comparing pytest *file names* and spot-checking that the
four twin-less legacy files looked folded into `test_interop.py` — which is
why it was written up as a confirmation task. The actual audit (#647) mapped
test *functions* and found 17 real gaps plus two missing binding surfaces
(now rows 3.7 and 3.8). The lesson generalises: for a row whose claim is
"coverage exists elsewhere", nothing short of an item-by-item mapping is
evidence. Rows asserting a file's *absence* (3.3, 3.4) or a symbol's absence
(2.1, 2.2) are sound as checked; rows asserting *equivalence* are not.

### 1. Hard code blockers

Nothing here is optional — each is something that stops the legacy tree from
being deletable, or stops next from being installable under the legacy name.

| # | Item | Why it blocks | Evidence | Size |
|:--:|---|---|---|:--:|
| 1.1 | ~~**Sever `wingfoil-next`'s dependency on the legacy `wingfoil` crate.**~~ **Done.** The shared core — `NanoTime`, `RunMode`/`RunFor`, `TimeQueue`, `Burst`/`burst!`, `Kernel`/`KernelWaker`/`ReadyReceiver`/`waker_channel`, and the whole latency data layer (`Traced`/`Latency`/`Stage`/`StageStats`/`LatencyStats`/`HasLatency`/`format_latency_report`/`record_stage_deltas`) — moved into `wingfoil-next/src/runtime/`, and `latency_stages!` moved from `wingfoil-derive` to `wingfoil-next-macros`. The edge is inverted: `wingfoil` now depends on `wingfoil-next` and re-exports every item at its historical path, so the classic API is unchanged and both engines share one set of types. `pub use wingfoil;` is gone from next's public API and the `nitro!` expansions name `::wingfoil_next::{RunMode, RunFor, Kernel, TimeQueue}` directly. `wingfoil-next-python`'s dependency went too. The only remaining edge is a **dev**-dependency for the parity tests and comparison benches, which cargo permits because it is dev-only and which goes away with the legacy tree. | — | `wingfoil/Cargo.toml` (depends on `wingfoil-next`); `next/crates/wingfoil-next/src/runtime/`; verified by `cargo tree -p wingfoil-next --edges normal` | — |
| 1.2 | **Crate + module rename.** `wingfoil-next` → `wingfoil`, `wingfoil-next-macros` → the derive crate, `wingfoil-next-python` → `wingfoil-python`, and the Python module `wingfoil_next` → `wingfoil`. | Cutover is a *name* takeover, not just a directory move. Touches every `use wingfoil_next::`, every doc link, every example, every workflow, and both publish jobs. Sequenced **after** 1.1, or the rename collides with the legacy crate of the same name. | port-plan.md:1489 names the module takeover but no mechanism | L |
| 1.3 | **Delete the `wingfoil-derive` crate** (`#[node]`). Drop the directory, the workspace member entry, and `wingfoil`'s dependency on it. | Already on the Phase 7 list; nothing under `next/` depends on it, so it is purely a consequence of the legacy tree going. | port-plan.md:1825 | S |
| 1.4 | **Retire the classic engine internals** (`MutableNode` wiring path) and rule on whether the classic facade API survives the swap. | Phase 7's first line, and the thing that decides whether Rust downstreams break. | port-plan.md:1819 | M |

### 2. Rulings owed — no code, but they gate the swap

The deviation register's standing instruction is that **every remaining 🔴 and
🟡 needs an explicit accept/fix ruling at cutover**. These are the open ones.

| # | Item | Class | Decision needed | Source |
|:--:|---|:--:|---|---|
| 2.1 | **`Graph::export` (GML topology dump) not ported.** The only *public* classic API next has no answer for. | ⚪ | Accept the drop and document it in the migration guide, or port `export` before the swap. | register C6 |
| 2.2 | **Latency ops are fluent/interpreted-only** — `stamp`/`stamp_precise`/`latency_report` have no `nitro!`/`compiled()`/`nested()` form. | ⚪ | Ratify as classic-parity (classic exposes latency only through `LatencyStreamOps`), or close the gap. | register C7 |
| 2.3 | **zmq cross-language interop not ported** — the `bincode` envelope is next-local, not wire-compatible with a classic/Python peer. Deferred "with the Python bindings (Phase 6)", and Phase 6 is now done, so the deferral has expired. | ⚪ | Re-rule now that its stated trigger has passed. | register C2 |
| 2.4 | **Live sources reject `RunMode::HistoricalFrom` at wiring.** Split ruling already agreed; the residual is that `kafka`/`fluvio` `_source` carry a live half only. | 🟡 | Confirm the bounded kafka/fluvio readers are *superset* work, not parity (classic never offered them), so they do not block. | register B2 |
| 2.5 | **`block_on` sinks/readers panic if the graph is driven from an async context.** | 🟡 | Accept as classic-parity + inherent to `block_on`-on-the-graph-thread. | register A5a |
| 2.6 | **`spawn_map` historical lock-step artifacts** — a filtering/delaying sub-graph desynchronises; bound runs by duration, not raw cycle count. | 🟢🟡 | Accept (classic's `graph_node` delay case fails likewise). | register B6 |
| 2.7 | **Capability-matrix 🟡s** — now **three**, not four: compiled realtime is timer-driven with no external wake (🟡³, verified — `compiled()` builds its own `Kernel::new(run_mode, run_for)` and drives `begin_cycle`/`end_cycle` with no waker channel); island dynamic-graph is partial (🟡¹⁰); compiled sparse gating is per-node `if` checks, not region gating (🟡¹³). ~~`stop`/`teardown` not emitted on compiled/island (🟡⁴)~~ — **stale, corrected**: both targets emit the full `start`/`stop`/`teardown` set for every node, error-safe at the loop tail, pinned by `tests/compiled_lifecycle_ops.rs`. Matrix row and footnote ⁴ updated. | 🟡 | Three accept-by-design rulings, or work items. | port-plan.md:65–74; macros `lib.rs:1606,2496–2548,2582–2593` |
| 2.8 | **C3 multi-output islands** and **C4 compiled-path IO ingestion** (busy-poll + bursts). | ⚪ | Confirm both stay post-v1; neither is a classic-parity gap (classic has no compiled tier). | register C3/C4 |

### 3. Superset gaps — Goal 1 explicitly names examples, benchmarks and docs

**Status: 3.1–3.6 are done and merged** — 3.1 → #645, 3.2 → #646, 3.3 → #644,
3.4 → #643, 3.5 → #648, 3.6 → #647. **3.7 and 3.8 are open**, and both were
discovered *by* 3.6 rather than being on the original list. Doing the work
changed what this section says, and the changes are recorded in place rather
than quietly edited away:

- **3.6 was not the confirmation task this row called it.** The audit mapped
  all 268 legacy test functions onto next's 325 and found 17 genuine test gaps
  (closed in #647) *and* two missing **binding surfaces** that no test can
  close. Those are now rows **3.7** and **3.8** below. The row's original
  "coverage does appear folded in — this is a confirmation task, not a known
  gap" was wrong.
- **3.1 and 3.2 were correctly scoped** but both grew a dependency the row did
  not anticipate: 3.2 had to port classic's `bench`-gated harness
  (`src/bencher.rs` + a `criterion` optional dep) as *source*, not just bench
  files.
- **All six were written before 1.1 landed**, and every one that added Rust
  reached for `use wingfoil::{NanoTime, RunFor, RunMode}` — the exact edge 1.1
  had just removed. `src/bencher.rs` would not have compiled at all under
  `--features bench`, since `wingfoil` is now only a dev-dependency. Repointed
  onto `wingfoil_next::` at merge. Worth knowing while 1.2 (the rename) is
  pending: **any branch cut before an invariant lands will reintroduce what it
  removed**, and CI on a stale base will not catch it.

| # | Item | Gap | Size |
|:--:|---|---|:--:|
| 3.1 | ~~**`latency_e2e` example not ported**~~ — **done (#645).** Re-verified before porting, and it stood. It is a *separate* example from `latency`: classic ships **both** `examples/latency/` (pub/sub/shared over an iceoryx2 hop) and `examples/latency_e2e/` (a nine-stage browser→ws_server→iceoryx2→fix_gw→FIX/TLS→LMAX round trip). Next has ported `latency/` only — `examples/latency/{README,pub.rs,shared.rs,sub.rs}` matches classic `examples/latency/` file-for-file, which is what the port plan's Phase 6 "landed" list refers to. `latency_e2e` has **no** next twin and no reference anywhere under `next/`. **Scope is smaller than it looks**: only ~1,200 lines are engine-coupled (`ws_server.rs` 399, `fix_gw.rs` 448, `shared.rs` 345, all on classic `wingfoil::` imports, declared as two `[[bin]]` targets in `wingfoil/Cargo.toml:388–394`). The rest — 5 Dockerfiles, docker-compose, Prometheus/Tempo/Grafana provisioning, the `static/` browser client, and three Pulumi stacks (fargate, ec2-spot, baremetal) — is engine-agnostic and **repoints** rather than ports, as do the three workflows that reference `wingfoil/examples/latency_e2e/` by path. | classic `wingfoil/examples/{latency,latency_e2e}/` vs next `examples/latency/`; `grep -rn latency_e2e next/` → nothing | M |
| 3.2 | ~~**Benchmark suite not ported**~~ — **done (#646).** All 8 classic targets ported (11 in next's manifest, counting the 3 next-only ones), same names and `required-features`, plus the `bench`-gated harness as source. Classic declares 8 bench targets — `graph` (needs the `bench` feature), `nanotime`, three `bfs_vs_dfs_*` (`wingfoil` / `reactive` / `async_streams`), `iceoryx2`, `iceoryx2_modes`, plus `aeron/`; next declares 3, none of them twins (`tiers`, `custom_op`, `store_baseline`). The port plan defers this per-adapter ("next's bench suite is a separate work item, as for every adapter so far") but Goal 1 names benchmarks in the superset claim. Note two of the `bfs_vs_dfs_*` targets benchmark *other libraries* (`reactive`, `async_streams`) as comparison baselines, so they are engine-agnostic and only need repointing. | `wingfoil/Cargo.toml:237–268` vs `next/crates/wingfoil-next/Cargo.toml:461–472` | M |
| 3.3 | ~~**Adapter directory `CLAUDE.md`: 15 legacy, 0 in next**~~ — **done (#644).** 18 files: the 15 twins, plus `lines` and `cache`, plus an index. Phase 4's own rule is "Each adapter: keep its directory CLAUDE.md". | `find … -name CLAUDE.md`: 15 vs 0 | M |
| 3.4 | ~~**Python API docs (Sphinx/readthedocs) have no next twin**~~ — **done (#643).** Mirrors the legacy layout, plus a `migration.rst` legacy has no equivalent of. `wingfoil-python/docs/` (`conf.py`, `api.rst`, `index.rst`, `readme.rst`, `requirements.txt`, Makefile) exists; `wingfoil-next-python/` has no `docs/` at all, and `.readthedocs.yaml` installs and builds from `wingfoil-python`. The docs build breaks at the swap. | `.readthedocs.yaml`; `ls next/crates/wingfoil-next-python/` | M |
| 3.5 | ~~**iceoryx2 `stages` latency path not bound in Python**~~ — **done (#648).** Was the last unported legacy Python capability. No longer blocked: `PyLatency::create_from_bytes` is exactly the header split it needs; what remains is wiring it into the two entry points and testing the round trip (no service needed). | port-plan.md:1687 | S |
| 3.6 | ~~**Verify `test_interop.py` really is at parity with the legacy pytest surface**~~ — **done, and it was not a formality (#647).** 268 legacy tests mapped onto next's 325 *by surface covered*, since only 6 of 268 names appear on both sides. 17 of the 21 legacy files have a same-named twin and every one is a superset. **17 genuine test gaps found and closed** — the largest being zmq, whose file docstring claimed a round trip that did not exist (every test was wiring-level) and whose `zmq_sub_etcd`/`zmq_pub_etcd` were bound but *wholly untested*; `zmq-next-integration.yml` previously ran Rust tests only and gained a Python leg. A live defect also fell out: `Stream.value()` **panicked** before the graph had run, escaping to Python as a bare `PanicException` — the earlier web-binding fix covered only the ran-but-never-ticked case, and classic `peek_value` answers `None` in both. Fixed. | PR #647; port-plan.md:1492 | — |
| 3.7 | **The statistics Python binding stops short of legacy.** Legacy `wingfoil-python/src/py_statistics.rs` binds `Window` / `Weighting` / `EwmaSpan` over `mean`/`std`/`var`/`sum`/`min`/`max`/`median`/`ewma`; next-python binds only cumulative `sum`/`mean`/`average`. **The engine already has the whole surface** (`wingfoil-next/src/stats.rs`, ported in Phase 2 with its own parity tests) — it is only the *binding* that is short, so this is binding work, not engine work. 37 legacy tests have nowhere to map. **Why it was missed:** legacy grew this binding *after* Phase 6's "Surface build-out ✅ landed" bullet was written, so the parity target moved under a bullet already marked done — worth remembering as a failure mode, since any legacy-side change after a ✅ does the same thing. | Found by the 3.6 audit (#647); `wingfoil-python/src/py_statistics.rs` vs `wingfoil-next-python/src/graph.rs` | M |
| 3.8 | **No multi-stream `build_dataframe` in next-python.** Legacy `pandas_helpers.build_dataframe` outer-joins several streams on time; next's `dataframe()` frames a single stream. 4 legacy tests have nowhere to map. Note the single-stream half is *better* in next (a real `pandas.DataFrame` built in Rust, where legacy returned `(time, value)` tuples for a Python helper to assemble), which is why the shape-coercion half of `test_pandas.py` is obsolete rather than missing — only the multi-stream join is a real gap. | Found by the 3.6 audit (#647) | S |

### 4. Docs the cutover itself owes

| # | Item | Note | Size |
|:--:|---|---|:--:|
| 4.1 | **Rewrite the crate-level docs.** `wingfoil-next/src/lib.rs` still opens *"**Design prototype**: what wingfoil's core abstractions look like if designed from scratch…"* and spends its first 40 lines as a post-mortem of the abandoned `wingfoil::codegen` retrofit. That is the front page of the crate the moment it becomes `wingfoil`. | `src/lib.rs:1–40` | M |
| 4.2 | **Migration guide `#[node]` → `Op`** — Phase 7 deliverable, does not exist yet. Must also carry the Python break (`import wingfoil` semantics change) and whatever 2.1 rules on `Graph::export`. | port-plan.md:1834 | M |
| 4.3 | **Root `README` / `CONTRIBUTING` / `CLAUDE.md` promotion.** `next/` mirrors all three, so the swap replaces the root copies — but the root `CLAUDE.md` is also the file describing the branching workflow that cutover ends. | `ls next/` vs `ls .` | S |
| 4.4 | **Architecture / orientation doc** (`docs/wingfoil-next-architecture.md`, was #507). Deliberately deferred until the refactor settled — it has, so this is now writable and cutover is exactly when a new contributor needs it. | port-plan.md:1971 | M |

### 5. Repo, CI and release plumbing — Goal 2's "directory promotion"

| # | Item | Note | Size |
|:--:|---|---|:--:|
| 5.1 | **Workspace `Cargo.toml`.** Members list drops `wingfoil`, `wingfoil-derive`, `wingfoil-python` and repoints the four `next/crates/*` entries to their promoted paths. `wingfoil-wire-types` stays (next's web adapter depends on it) and `wingfoil-wasm` stays excluded. | `Cargo.toml:2–18`; `wingfoil-next/Cargo.toml:112` | S |
| 5.2 | **Collapse the workflow set.** Of 42 workflows, **14 carry `next`** (12 `*-next-integration`, plus `next-python-test`) and lose the `-next`; the legacy-side twins retire (`adapter-integration`, `integration-tests`, `augurs-integration`, `aeron-integration`, `etcd-`, `iceoryx2-`, `kdb-`, `otlp-`, `postgres-`, `prometheus-`, `redis-`, `web-`, `zmq-etcd-`, `kafka-python-`, `python-test`, …). Legacy `augurs-integration.yml` has no next twin **by design, not as a gap** — next's augurs tests run inside `rust-test.yml`'s `test-next` job under `--all-features`. The three `latency-e2e.*` workflows repoint with 3.1. | `.github/workflows/` (42 files, 14 matching `next`) | M |
| 5.3 | **`crates-publish.yml` rewrite.** Publishes by directory in dependency order (`wingfoil-derive` → `wire-types` → `wingfoil` → `wingfoil-python`) with crates.io index waits between. The crate set, the order and the paths all change. | `.github/workflows/crates-publish.yml:23–72` | M |
| 5.4 | **`pypi-publish.yml` repoint**, plus a ruling on the wheel's adapter roll-up: aeron is out of both roll-ups (builds a C library) and iceoryx2 is in `all-adapters` but out of the wheel (Linux/POSIX-only). That reproduces open issue **#367**, so decide whether cutover inherits it or fixes it. | workflow lines 59–123; port-plan.md:1666–1706 | S |
| 5.5 | **`.readthedocs.yaml` repoint** — paired with 3.4; today it pip-installs `wingfoil-python` and builds `wingfoil-python/docs/conf.py`. | `.readthedocs.yaml` | S |
| 5.6 | **Major version bump** and the `rust-test`/`all-tests`/`rust-fmt` path and branch filters (currently `[main, next]`). | port-plan.md:1836 | S |

### 6. Verification gates — run immediately before the swap

| # | Gate |
|:--:|---|
| 6.1 | `cargo fmt --all -- --check`, `cargo lint`, `cargo lint-all` green on the promoted tree. |
| 6.2 | `cargo test -p wingfoil-next --all-features` and the next-python pytest suite green. |
| 6.3 | Every `*-next-integration` workflow green on the cutover branch (they gate the service-backed adapters that the unit suites cannot). |
| 6.4 | `cargo bench --bench tiers` re-read: the `next-interpreted ≥ classic-interpreted` baseline is the last moment it can be checked, since the classic bar disappears with the legacy tree. Wiring it as an automated CI gate stays deliberately deferred (criterion wall-clock is too noisy on shared runners) — so this is a manual read. |

### Open issues to route

Only **#602** (aeron: fragment assembly, sub ergonomics, publisher
back-pressure) carries the `next` label. The remaining 31 open issues are
`classic`-labelled; most die with the legacy tree, but these survive the swap
and should be re-labelled rather than closed: **#367** (wheel excludes
aeron/iceoryx2 — see 5.4), **#450** (no manylinux/aarch64/sdist wheels,
trusted publishing), **#452** (Dependabot alerts, wasm lockfile), **#449 /
#451 / #359** (CI blind spots, workflow dedup, stale actions), **#461**
(supply-chain hardening), **#457** (wingfoil-js), and **#437** (web historical
streaming is lossy — confirm whether next's web adapter already fixes it).

### Suggested order

**1.1 is done** (see the row above), so **1.2 — the crate + module rename — is
now the head of the critical path.** 1.1 had to land first because the rename
collides with the legacy crate name otherwise; with the shared core moved and
the edge inverted, nothing named `wingfoil` is in next's library graph and
1.2 is unblocked. Both were prerequisites for the directory promotion itself.
Section 2's rulings need no code and can be settled in parallel — but 2.1
feeds the migration guide (4.2), so it wants to land early. Section 3's
superset gaps are independent of each other. Section 5 can only be done once
1.2 fixes the names.
