# The final cutover — runbook

Everything else is done. This is the sequence that removes the legacy tree and
the scaffolding that existed only to let the two engines coexist.

[`cutover-plan.md`](cutover-plan.md) holds the *why* and the audit trail of
rulings; this file is the *how*, written to be executed. Figures are counted
from `next` @ `f769d56` — re-count before starting, since they drift with any
merge.

## What is and is not reversible

Worth being precise, because "irreversible" is doing a lot of work in
conversation about this step.

**Recoverable.** The legacy source is in git history forever, and
`wingfoil` 8.0.0 / `wingfoil-derive` 8.0.0 / `wingfoil-python` 8.0.0 /
`wingfoil-wire-types` 8.0.0 stay on crates.io permanently — crates.io does not
delete. Existing lockfiles keep resolving. Nothing a downstream user has today
stops working.

**Not recoverable.** The *comparison* between the engines. Once legacy is gone
you cannot run a legacy-vs-next benchmark or a cross-engine wire test again
without reviving the tree. That is why **gate 6.4 was read and its numbers
written into `cutover-plan.md` before this step** — that capture is the
permanent record, and re-running it later is not an option.

Everything else here is mechanical and re-doable.

## Pre-flight

Run these before touching anything. Stop if any fails.

```bash
# 6.5 — legacy drift sweep. Anything legacy-originated since the 3.9 sweep is a
# parity target that has to be dealt with BEFORE deletion, not after.
git log --format='%h %ad %s' --date=short 754514c..HEAD -- legacy/

# CI green on next, and the working tree clean.
git status --porcelain
```

The sweep is expected to return only cutover-mechanics commits (the workspace
split, the rename alias, `legacy/.gitignore`). Anything else — a real change to
legacy source — means someone was still working in that tree.

Land the deletion as **one PR with the tree quiet**, for the same reason the
rename was: it touches everything, so anything open across it conflicts.

---

## Step 1 — delete the tree

```bash
git rm -r legacy/
```

That is cutover-plan **1.3** (the legacy `wingfoil-derive` crate) and the
deletion half of **4.3** (the `legacy/` copies of README / CONTRIBUTING /
CLAUDE.md) in one move. Nothing under `crates/` depends on the legacy crates —
that invariant has been enforced since the dependency inversion, and it is what
makes this a deletion rather than an unpick.

## Step 2 — remove the `legacy_wingfoil` alias

The alias existed because a package cannot depend on another of its own name.
With legacy gone there is nothing to alias.

**`crates/wingfoil/Cargo.toml`** — three entries:

| line | what | action |
|---|---|---|
| dev-dep | `legacy_wingfoil = { package = "wingfoil", path = "../../legacy/wingfoil" }` | delete |
| `iceoryx2` feature | `"legacy_wingfoil/iceoryx2"` | drop from the list |
| `zmq-cross-engine-test` feature | `"zmq", "legacy_wingfoil/zmq"` | delete the whole feature |

**Three files use it, and all three are deletions, not rewrites** — each exists
to compare against an engine that no longer exists:

- `crates/wingfoil/tests/engine_semantics.rs` — the parity oracle.
- `crates/wingfoil/tests/zmq_cross_engine_integration.rs` — proved the two
  engines agree on the wire. Its sibling
  `zmq_cross_lang_integration.rs` **stays**: that one tests Rust ↔ Python,
  which survives the cutover.
- `crates/wingfoil/benches/tiers.rs` — the `legacy` arm of each group only.
  **The bench itself stays**; strip the legacy bars and their imports, keeping
  interpreted / compiled / nested. Also drop its `[[bench]]`-adjacent legacy
  references and the `legacy` group labels.

Do not delete `tiers.rs` wholesale — the three surviving tiers are still the
tier-comparison benchmark.

## Step 3 — revert the package-selection workaround

`-p wingfoil` was ambiguous only because two packages carried that name. Verify
it is no longer, then revert:

```bash
cargo check -p wingfoil --lib      # must now resolve, not error
```

- **20 workflow lines** and **73 docs/skills files**:
  `--manifest-path crates/wingfoil/Cargo.toml` → `-p wingfoil`.
- **28 lines** referencing `--manifest-path legacy/...` — these go with the
  workflows that own them (step 4) or are docs that die with the tree.
- Restore `-p wingfoil-python` where it was rewritten to a manifest path.

Take the same care the rename needed: the pattern is `-p wingfoil(?![-\w])`.
A bare `-p wingfoil\b` **also matches `-p wingfoil-python`**, because a hyphen
is a word boundary — that mistake cost a CI round during 1.2.

## Step 4 — collapse the workflow set (5.2)

Now the filenames are free. Fourteen lose the suffix:

| from | to |
|---|---|
| `aeron-next-integration.yml` | `aeron-integration.yml` |
| `etcd-next-integration.yml` | `etcd-integration.yml` |
| `fix-next-integration.yml` | `fix-integration.yml` |
| `fluvio-next-integration.yml` | `fluvio-integration.yml` |
| `iceoryx2-next-integration.yml` | `iceoryx2-integration.yml` |
| `kafka-next-integration.yml` | `kafka-integration.yml` |
| `kdb-next-integration.yml` | `kdb-integration.yml` |
| `next-python-test.yml` | `python-test.yml` |
| `otlp-next-integration.yml` | `otlp-integration.yml` |
| `postgres-next-integration.yml` | `postgres-integration.yml` |
| `prometheus-next-integration.yml` | `prometheus-integration.yml` |
| `redis-next-integration.yml` | `redis-integration.yml` |
| `web-next-integration.yml` | `web-integration.yml` |
| `zmq-next-integration.yml` | `zmq-integration.yml` |

Fourteen legacy-side workflows retire: `adapter-`, `aeron-`, `augurs-`,
`etcd-`, `iceoryx2-`, `kafka-python-`, `kdb-`, `otlp-`, `postgres-`,
`prometheus-`, `redis-`, `web-`, `zmq-etcd-integration.yml`, and
`python-test.yml`.

Delete the legacy ones **in the same commit** as the renames, or git will see a
collision.

Also update, in each renamed file: the `name:` field (`test.integration.zmq-next`
→ `test.integration.zmq`), the `concurrency.group` literal, and the `paths:`
filters. Then fix the references in `integration-tests.yml` and `all-tests.yml`.

**`augurs-integration.yml` has no next twin by design** — next's augurs tests
run inside `rust-test.yml` under `--all-features`. Retire it; do not rename it.

> ⚠️ **Check names change.** Renaming a workflow renames its CI check. If the
> repository has required status checks configured on `main` or `next`, they
> must be updated in the same window or merges will block on checks that can
> never report. This is the one step with a consequence outside the repo.

## Step 5 — docs

- Root `CLAUDE.md`: remove the legacy branching section and the
  "Working under `legacy/`?" banner. The two-branch workflow (`main` for
  legacy, `next` for everything else) ends here — there is only one tree.
- `docs/migration.md`: keep it. It is for users migrating *off* the legacy
  engine, and is more useful after the deletion, not less.
- `crates/README.md` and the architecture doc: drop the `legacy/` row and any
  "parity oracle" framing that is now historical.
- `docs/port-plan.md` / `cutover-plan.md`: mark Phase 7 complete. Keep the
  rulings and the gate 6.4 numbers — that is the audit trail.

## Step 6 — gates on the promoted tree

```bash
cargo fmt --all -- --check          # 6.1
cargo lint
cargo lint-all                      # needs aeron's toolchain: cmake >= 3.30
cargo test -p wingfoil --all-features   # 6.2
cd crates/wingfoil-python && maturin develop && pytest
```

Read exit codes directly — piping into `head`/`tail` masks them.

**6.3**: every integration workflow green on the cutover branch. They gate the
service-backed adapters the unit suites cannot reach.

**6.4** is already banked in `cutover-plan.md` and cannot be re-run. Do not
treat its absence from this list as an oversight.

## Step 7 — the swap itself

Not in the plan's §5/§6 lists, and easy to forget: everything so far has landed
on `next`. `main` still carries the pre-cutover world.

1. Open the `next` → `main` PR. This is the swap.
2. Update the `[main, next]` branch filters in `rust-test.yml`,
   `all-tests.yml` and `rust-fmt.yml` — the residual of 5.6.
3. Decide `next`'s fate: retire it, or keep it as the integration branch with
   `main` as release. Whichever, say so in `CONTRIBUTING.md`, because the
   branching rules in `CLAUDE.md` currently describe a world with two trees.

## Step 8 — issues

**The re-labelling is already done** — all 26 open issues carry `next`, and
nothing is left under `classic`. What this step still owes:

- **Close #367** (iceoryx2/aeron missing from the wheel) — resolved by the
  5.4 wheel change.
- Re-check the survivors against the deleted tree: **#450** wheels, **#452**
  dependabot, **#449 / #451 / #359** CI, **#461** supply chain, **#457**
  wingfoil-js, **#437** web historical streaming. All describe the surviving
  engine or its packaging, so they stay open — but **#437** in particular
  should be confirmed against next's web adapter rather than assumed to carry
  over, and the CI issues (#449 / #451) are partly answered by step 4's
  workflow collapse.
- Anything that still describes the *deleted* engine can be closed with a note
  pointing at this runbook.

## Order

Steps 1–4 are one PR: each breaks the tree on its own, and only the combination
compiles. 5 can ride along. 6 gates it. 7 is a separate PR by nature. 8 is
independent and can happen any time.
