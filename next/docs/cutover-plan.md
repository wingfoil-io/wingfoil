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

### 1. Hard code blockers

Nothing here is optional — each is something that stops the legacy tree from
being deletable, or stops next from being installable under the legacy name.

| # | Item | Why it blocks | Evidence | Size |
|:--:|---|---|---|:--:|
| 1.1 | **Sever `wingfoil-next`'s dependency on the legacy `wingfoil` crate.** Next imports its most load-bearing primitives *from* the tree it is meant to replace: `NanoTime`, `RunMode`, `RunFor`, `TimeQueue`, `Burst`/`burst`, `codegen::{Kernel, KernelWaker, ReadyReceiver, waker_channel}`, the whole latency payload set (`Traced`/`Latency`/`Stage`/`StageStats`/`LatencyStats`/`HasLatency`/`latency_stages!`/`format_latency_report`/`record_stage_deltas`) and `nodes::DropSmallChangeStream`. These must move into next (or a shared base crate) first. | The legacy tree cannot be deleted while next compiles against it. This is **not currently on the Phase 7 checklist** — it is the largest unlisted prerequisite. | `next/crates/wingfoil-next/Cargo.toml:16`, `next/crates/wingfoil-next-python/Cargo.toml:130` (both `path = "../../../wingfoil"`); `src/lib.rs:137`, `src/latency.rs:82` | L |
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
| 2.7 | **Capability-matrix 🟡s**: compiled realtime is timer-driven only (🟡³); `stop`/`teardown` not emitted on compiled/island (🟡⁴); island dynamic-graph is partial (🟡¹⁰); compiled sparse gating is per-node `if` checks, not region gating (🟡¹³). | 🟡 | Four separate accept-by-design rulings, or work items. | port-plan.md:65–74 |
| 2.8 | **C3 multi-output islands** and **C4 compiled-path IO ingestion** (busy-poll + bursts). | ⚪ | Confirm both stay post-v1; neither is a classic-parity gap (classic has no compiled tier). | register C3/C4 |

### 3. Superset gaps — Goal 1 explicitly names examples, benchmarks and docs

| # | Item | Gap | Size |
|:--:|---|---|:--:|
| 3.1 | **`latency_e2e` example not ported.** The one classic example with no next twin — a Docker/AWS-deployed end-to-end latency harness (fix gateway, ws server, Prometheus, Tempo, Grafana) with **three dedicated workflows** (`build-latency-e2e-ami`, `build-latency-e2e-images`, `deploy-latency-e2e`). Every other classic example has a next twin. | classic `wingfoil/examples/latency_e2e/` vs next `examples/` | L |
| 3.2 | **Benchmark suite not ported.** Classic has `graph.rs`, `nanotime.rs`, `bfs_vs_dfs/`, `iceoryx2.rs`, `iceoryx2_modes.rs`, `aeron/`; next has only `tiers.rs`, `custom_op.rs`, `store_baseline.rs`. The port plan defers this per-adapter ("next's bench suite is a separate work item, as for every adapter so far") but Goal 1 names benchmarks in the superset claim. | `wingfoil/benches/` vs `next/crates/wingfoil-next/benches/` | M |
| 3.3 | **Adapter directory `CLAUDE.md`: 15 legacy, 0 in next.** Phase 4's own rule is "Each adapter: keep its directory CLAUDE.md". | `find … -name CLAUDE.md`: 15 vs 0 | M |
| 3.4 | **Python API docs (Sphinx/readthedocs) have no next twin.** `wingfoil-python/docs/` (`conf.py`, `api.rst`, `index.rst`, `readme.rst`, `requirements.txt`, Makefile) exists; `wingfoil-next-python/` has no `docs/` at all, and `.readthedocs.yaml` installs and builds from `wingfoil-python`. The docs build breaks at the swap. | `.readthedocs.yaml`; `ls next/crates/wingfoil-next-python/` | M |
| 3.5 | **iceoryx2 `stages` latency path not bound in Python** — the one legacy Python capability still unported. No longer blocked: `PyLatency::create_from_bytes` is exactly the header split it needs; what remains is wiring it into the two entry points and testing the round trip (no service needed). | port-plan.md:1687 | S |
| 3.6 | **Verify `test_interop.py` really is at parity with the legacy pytest surface.** That is the *stated Phase 6 gate*, and legacy has four test files with no same-named twin (`test_pandas`, `test_statistics`, `test_streams`, `test_web_bindings`). Coverage does appear folded into `test_interop.py`/`test_examples.py` — this is a confirmation task, not a known gap. | port-plan.md:1492 | S |

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
| 5.2 | **Collapse the workflow set.** ~20 legacy-side workflows retire (`adapter-integration`, `integration-tests`, `augurs-integration`, `aeron-integration`, `etcd-`, `iceoryx2-`, `kdb-`, `otlp-`, `postgres-`, `prometheus-`, `redis-`, `web-`, `zmq-etcd-`, `kafka-python-`, `python-test`, …) and ~13 `*-next-integration` workflows lose the `-next`. Legacy `augurs-integration.yml` has no next twin by design — next's augurs tests run inside `rust-test.yml`'s `test-next` job under `--all-features`. | `.github/workflows/` (42 files) | M |
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

1.1 → 1.2 are the critical path and everything else can run beside them: 1.1
is a prerequisite for 1.2 (the rename collides with the legacy crate name
otherwise), and both are prerequisites for the directory promotion itself.
Section 2's rulings need no code and can be settled in parallel — but 2.1
feeds the migration guide (4.2), so it wants to land early. Section 3's
superset gaps are independent of each other. Section 5 can only be done once
1.2 fixes the names.
