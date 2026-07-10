# Handover — Codebase Review & Improvement Plan

*Session date: 2026-07-08/10. Branch: `claude/codebase-review-plan-qrp8m4`,
based on `main` at `6be91d4` ("bump: major version to 7.0.0"). No product code
was changed — this branch adds documentation only.*

## What this branch contains

| File | Purpose |
|------|---------|
| `docs/IMPROVEMENT_PLAN.md` | Full five-phase improvement plan from a whole-codebase review (core engine, all node operators, all 18 adapters, Python/JS/wasm bindings, all 28 CI workflows). Every item cites `file:line`. |
| `docs/HANDOVER.md` | This file. |

## How the review was done

Five parallel deep-review passes (core engine; nodes/operators; adapters;
bindings; CI/docs/tests), followed by manual verification of the
highest-severity claims. Baseline health was checked directly:
`cargo build` (full workspace) and `cargo test -p wingfoil` both pass —
**note that a fresh machine needs `protoc` installed**
(`apt-get install protobuf-compiler`) or the workspace build fails in
`etcd-client`'s build script (pulled in via wingfoil-python's features).

## Findings verified by hand (not just reported)

These four were independently confirmed against the source or by experiment.
Treat them as established, not hypotheses:

### 1. `csv_read` drops the last row of every file — CONFIRMED by failing test
Reproduced with a plain 3-row CSV (no sentinel):

```rust
// wingfoil/tests/last_row_repro.rs (was created, run, then deleted — recreate to reproduce)
std::fs::write(&path, "1000,1\n2000,2\n3000,3\n").unwrap();
let stream = csv_read::<(NanoTime, u32)>(path, |r| r.0, false).unwrap();
// ... run historical, collect ...
// Result: [1, 2]  — expected [1, 2, 3]
```

Root cause: `IteratorStream::cycle` / `TryIteratorStream::cycle`
(`wingfoil/src/nodes/iterator_stream.rs:30-47,105-124`) buffer the current
time's items into `self.value`, then return `add_callback(...)`, which is
`Ok(false)` once the iterator is exhausted — so the node reports "didn't tick"
on exactly the cycle that holds the final burst. The project's own tests and
the checked-in fixture `wingfoil/src/adapters/csv/test_data/read_test.csv`
work around it with sentinel rows (see the comments at
`iterator_stream.rs:217-219` and `csv/read.rs:66-68`).

**Fix shape:** return `Ok(!self.value.is_empty())` and schedule the next
callback independently of the return value. Then remove the sentinels from the
tests/fixtures so they pin the fix. Affects `IteratorStream`,
`TryIteratorStream`, and `SimpleIteratorStream` (three copies of the pattern
in the same file).

### 2. `release.yml` publishes the Python package to Test PyPI — CONFIRMED mechanically
`release.yml` is `workflow_dispatch` with **no inputs** and calls
`pypi-publish.yml` with `pypi-target: prod`. But `pypi-publish.yml:84` reads
`${{ github.event.inputs.pypi-target }}` — under `workflow_call`,
`github.event` is the *caller's* trigger event, whose inputs are empty — so
the `prod` parameter is never read and the `else` branch (Test PyPI) always
wins. Consistent with observed state: real PyPI's latest is 6.0.5 while the
repo is at 7.0.0; 6.0.5 presumably reached prod via a manual
`workflow_dispatch` of `pypi-publish.yml` itself (the one path that works).

**Fix shape:** one-line change to `${{ inputs.pypi-target }}` (populated for
both `workflow_call` and `workflow_dispatch`).

### 3. `NanoTime::now()` is monotonic, not epoch time — CONFIRMED in source
`wingfoil/src/time.rs:45-47` reads `quanta::Clock::now()` (CLOCK_MONOTONIC —
nanoseconds since boot), while the type documents "nanoseconds since the unix
epoch" (`time.rs:17`). Every timestamp persisted in `RunMode::RealTime`
(kdb/Postgres/CSV writes, cross-host latency stamps) is boot-relative — i.e.
a 1970-era date. The latency test at `latency.rs:823-826` only asserts `> 1s`,
which masks it.

**Fix shape:** snap `SystemTime::now()` against `Clock::now()` once at startup
(LazyLock offset), apply in `now()`; add a test asserting `now()` ≈
`SystemTime::now()`. Consider upgrading `quanta` 0.9.3 → 0.12.x at the same
time.

### 4. Bare `.unwrap()`s in the production async channel path — CONFIRMED in source
`wingfoil/src/channel/kanal_chan.rs:240,276,279,281` — panics on normal
shutdown races instead of surfacing `SendNodeError::ChannelClosed` like the
sync path (`kanal_chan.rs:194-208`) does.

## The rest of the plan

`docs/IMPROVEMENT_PLAN.md` has the full inventory, phased by priority:

- **Phase 0** — the four items above plus kdb q-injection in `kdb_write` and
  Postgres passwords leaking into error messages. Small, independent PRs.
- **Phase 1** — engine/operator correctness (TimeQueue silently dedupes
  events; filter semantics differ between Stream and Node; `Graph::run()`
  skips stop/teardown on error; recursion in graph wiring; FIX partial-frame
  corruption; untested `dynamic_group`).
- **Phase 2** — CI: workflow-only PRs run zero CI; JS tests never run;
  release tags before publishing; no manylinux/sdist/x86_64-mac wheels;
  26 open Dependabot alerts (1 critical, 6 high); workflow consolidation.
- **Phases 3–5** — consolidation (try_* twins, Stream/Node operator pairs,
  shared adapter infra incl. bounded channels), bindings quality (UB
  transmutes in wingfoil-python, keyword-named methods), docs/API polish.

Phase 0 + early Phase 1 are the "genuine bugs with user-visible data loss or
broken releases" tier — roughly two weeks of small PRs. Phases 2–5 are
long-term-investment material; the maintainer may reasonably defer them.

**Not independently verified** (reported by review passes with `file:line`
evidence but not re-derived by hand): everything outside the four items above.
Spot-check before acting on any single claim — in particular the adapter and
bindings findings.

## Caveats / owner input wanted

- The maintainer's initial reaction was that the findings "sound pretty
  trivial/pedantic" — the demonstration above was produced in response. The
  four verified items stand; the phasing of everything else is negotiable.
- The sentinel-row workarounds suggest the last-item drop may be *known*
  upstream behavior. If it's intentional, the right fix is still to make
  `csv_read` correct (users won't add sentinels), but check for related
  design discussion/issues before changing `IteratorStream` semantics.
- No GitHub issues were filed and no PR was opened for this branch — the
  user did not ask for either.

## Practical notes for the next session

- Branch pushed to `origin/claude/codebase-review-plan-qrp8m4`; working tree
  clean; nothing else in flight.
- The repo has a pre-commit hook (cargo-husky) that runs fmt/clippy/tests —
  commits are slow but self-checking. CI mirror: `cargo lint` and
  `cargo lint-all` (aliases in `.cargo/config.toml`); `lint-all` needs
  `protoc`.
- `pytest` for wingfoil-python needs `maturin develop` first (see CLAUDE.md).
- Verify fixes with the repro pattern above rather than the existing unit
  tests — several existing tests encode the buggy behavior (sentinels,
  `merge_last_ticked_value_wins`, the no-assertion `sample` test).
