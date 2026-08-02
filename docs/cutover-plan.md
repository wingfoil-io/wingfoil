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
| 1.3 | **Delete the `wingfoil-derive` crate.** It now holds only `#[node]`. Drop the directory, the workspace member entry, and `wingfoil`'s dependency on it. | Nothing under `crates/` depends on it; removal is purely a consequence of the legacy tree going. | S |
| 1.4 | **Retire the legacy engine internals** (`MutableNode` wiring path) and rule on whether the legacy facade API survives the swap. | Decides whether Rust downstreams break at the version bump. | M |

### 2. Rulings owed — no code, but they gate the swap

The deviation register's standing instruction: **every remaining 🔴 and 🟡
needs an explicit accept/fix ruling at cutover.** These are the open ones.

| # | Item | Class | Decision needed | Source |
|:--:|---|:--:|---|---|
| 2.1 | **`Graph::export` (GML topology dump) not ported.** The only *public* legacy API next has no answer for. | ⚪ | Accept the drop and document it in the migration guide, or port `export` before the swap. Feeds 4.2. | register C6 |
| 2.2 | **Latency ops are fluent/interpreted-only** — `stamp`/`stamp_precise`/`latency_report` have no `nitro!`/`compiled()`/`nested()` form. | ⚪ | Ratify as legacy-parity (legacy exposes latency only through `LatencyStreamOps`), or close the gap. | register C7 |
| 2.3 | **zmq cross-language interop not ported** — the `bincode` envelope is next-local, not wire-compatible with a legacy/Python peer. Its stated deferral was "with the Python bindings (Phase 6)", which is now done. | ⚪ | Re-rule now the deferral has expired. | register C2 |
| 2.4 | **Live sources reject `RunMode::HistoricalFrom` at wiring.** Split ruling already agreed; the residual is that `kafka`/`fluvio` `_source` carry a live half only. | 🟡 | Confirm the bounded kafka/fluvio readers are *superset* work, not parity (legacy never offered them), so they do not block. | register B2 |
| 2.5 | **`block_on` sinks/readers panic if the graph is driven from an async context.** | 🟡 | Accept as legacy-parity and inherent to `block_on`-on-the-graph-thread. | register A5a |
| 2.6 | **`spawn_map` historical lock-step artifacts** — a filtering/delaying sub-graph desynchronises; bound runs by duration, not raw cycle count. | 🟢🟡 | Accept (legacy's `graph_node` delay case fails likewise). | register B6 |
| 2.7 | **Three capability-matrix 🟡s**: compiled realtime is timer-driven with no external wake (🟡³); island dynamic-graph is partial (🟡¹⁰); compiled sparse gating is per-node `if` checks, not region gating (🟡¹³). | 🟡 | Three accept-by-design rulings, or work items. | port-plan.md capability matrix |
| 2.8 | **C3 multi-output islands** and **C4 compiled-path IO ingestion** (busy-poll + bursts). | ⚪ | Confirm both stay post-v1; neither is a legacy-parity gap, since legacy has no compiled tier. | register C3/C4 |

### 3. Superset gaps — Goal 1 names examples, benchmarks, bindings and docs

| # | Item | Size |
|:--:|---|:--:|
| 3.7 | **The statistics Python binding stops short of legacy.** Legacy `legacy/wingfoil-python/src/py_statistics.rs` binds `Window` / `Weighting` / `EwmaSpan` over `mean`/`std`/`var`/`sum`/`min`/`max`/`median`/`ewma`; next-python binds only cumulative `sum`/`mean`/`average`. The engine already has the whole surface (`crates/wingfoil-next/src/stats.rs`, with parity tests), so this is binding work, not engine work. 37 legacy tests have nowhere to map. | M |
| 3.8 | **No multi-stream `build_dataframe` in next-python.** Legacy `pandas_helpers.build_dataframe` outer-joins several streams on time; next's `dataframe()` frames a single stream. 4 legacy tests have nowhere to map. The single-stream half is already ahead of legacy (a real `pandas.DataFrame` built in Rust), so only the multi-stream join is missing. | S |
| 3.9 | **Sweep the legacy tree for drift since each phase was ticked.** 3.7 exists because legacy grew `py_statistics.rs` *after* Phase 6's "Surface build-out ✅" was written — the parity target moved under a bullet already marked done, and nothing detects that. `git log wingfoil/ wingfoil-python/` since each phase's completion date will show whether 3.7 is the only case. Until this runs, every ✅ in `port-plan.md` is a claim about the tree as it was, not as it is. | M |

### 4. Docs the cutover owes

| # | Item | Size |
|:--:|---|:--:|
| 4.1 | **Rewrite the crate-level docs.** `crates/wingfoil-next/src/lib.rs` still opens *"**Design prototype**: what wingfoil's core abstractions look like if designed from scratch…"* and spends its first 40 lines on a post-mortem of the abandoned `codegen` retrofit. That becomes the crate's front page the moment it is `wingfoil`. | M |
| 4.2 | **Migration guide `#[node]` → `Op`.** Does not exist. Must also carry the Rust facade decision (1.4) and whatever 2.1 rules on `Graph::export`. The Python half is largely written — `wingfoil-next-python/docs/migration.rst` — and can be referenced rather than duplicated. | M |
| 4.3 | **Retire the `legacy/` copies of `README` / `CONTRIBUTING` / `CLAUDE.md`.** The promotion itself is done — next's copies *are* the root copies and the originals moved to `legacy/`. What is left for cutover is deleting them with the rest of the tree, and stripping the now-moot legacy branching section from the root `CLAUDE.md`. | S |
| 4.4 | **Architecture / orientation doc** (`docs/wingfoil-next-architecture.md`, was #507). Deferred until the refactor settled; it has, and cutover is exactly when a new contributor needs it. | M |

### 5. Repo, CI and release plumbing — Goal 2's "directory promotion"

All of this is blocked on 1.2, which fixes the names everything here refers to.

| # | Item | Size |
|:--:|---|:--:|
| 5.1 | **Workspace `Cargo.toml`.** Drop the three `legacy/*` members. The four next crates are already at `crates/*`, so no repointing is left. `crates/wingfoil-wire-types` stays (next's web adapter depends on it); `crates/wingfoil-wasm` stays excluded. | S |
| 5.2 | **Collapse the workflow set.** 14 of 42 workflows carry `next` and lose the suffix; the legacy-side twins retire. The three `latency-e2e.*` workflows are already repointed at `crates/wingfoil-next/examples/latency_e2e/`, so they need nothing at cutover. Note `augurs-integration.yml` has no next twin **by design** — next's augurs tests run in `rust-test.yml`'s `test-next` job under `--all-features`. | M |
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

**1.2, the crate + module rename, is the head of the critical path.** It
touches every `use wingfoil_next::` in the tree, so anything open across it
conflicts; land it with nothing else in flight. Section 5 can only start
afterwards, since it repoints the names 1.2 creates.

Everything else parallelises. Section 2's rulings need no code — 2.1 and 1.4
should land early because 4.2 depends on both. Sections 3 and 4 are
independent of each other and of 1.2. **3.9 is worth running before anything
else in section 3**, since it may add rows to it.

One sequencing hazard, learned the hard way: a branch cut before an
invariant lands will happily reintroduce what that invariant removed, and CI
on a stale base will not catch it. Rebase onto `next` before merging anything
that has been open across a structural change.
