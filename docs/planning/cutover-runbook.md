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
you cannot run a legacy-vs-wingfoil benchmark or a cross-engine wire test again
without reviving the tree. That is why **gate 6.4 was read and its numbers
written into `cutover-plan.md` before this step** — that capture is the
permanent record, and re-running it later is not an option.

Everything else here is mechanical and re-doable.

## Pre-flight

Run these before touching anything. Stop if any fails.

```bash
# 6.5 — legacy drift sweep. Anything legacy-originated since the 3.9 sweep is a
# parity target that has to be dealt with BEFORE deletion, not after.
git log --format='%h %ad %s' --date=short 73146b8..HEAD -- legacy/

# CI green on wingfoil, and the working tree clean.
git status --porcelain
```

The sweep is expected to return only cutover-mechanics and docs commits (the
workspace split, the rename alias, `legacy/.gitignore`, licence and link
housekeeping). Anything else — a real change to legacy source — means someone
was still working in that tree.

> **The anchor moved, and will again.** The sweep was written against
> `754514c`, the tree inversion (#655). That SHA **no longer resolves**: the
> port line was re-parented onto `main`, so the inversion's current identity is
> `73146b8`, the oldest commit that touches `legacy/` in today's history —
> which is the anchor above. Re-derive it rather than trusting it if history is
> rewritten again:
> `git log --format='%h %ad %s' --date=short --diff-filter=A -- legacy/CLAUDE.md`.
> `cutover-plan.md` §3.9 still cites `754514c` throughout; that is a historical
> record of a sweep that ran against the pre-rebase tree, and is left alone
> deliberately.

**Ran 2026-08-12** against `a6aae88`: eight commits, all docs, licence text,
naming and cutover mechanics. No legacy-originated functional change since
`da919bb` (2026-08-01), so §3 gains no rows.

Land the deletion as **one PR with the tree quiet**, for the same reason the
rename was: it touches everything, so anything open across it conflicts.

---

## Step 0 — the extraction ✅ **done**

Four things lived only under `legacy/` while surviving code and CI pointed at
them. They were copied out ahead of the deletion, so step 1 is now purely
subtractive:

| What | To | Why it could not wait |
|---|---|---|
| `src/adapters/kdb/docker/` (Dockerfile + `q` + `q.k`) | `crates/wingfoil/src/adapters/kdb/docker/` | **wingfoil's own** `kdb-integration.yml` built the legacy context — KDB+ has no public licensed image |
| `src/adapters/prometheus/docker/` | *not copied* — `prometheus-integration.yml` repointed at `crates/wingfoil/examples/adapters/telemetry/docker/` | same break; the telemetry example already carries an identical stack, so this is a repoint, not a duplicate |
| `benches/bfs_vs_dfs/latency.png` | `crates/wingfoil/benches/topological_vs_per_path/legacy_engine_latency.png` | the surviving README linked it, and it cannot be regenerated once the engine is gone |
| `examples/order_book/data/aapl_readme.txt` | `crates/wingfoil/examples/core/order_book/data/` | LOBSTER's attribution for `aapl.csv`, which survives |

Also landed with it: the ten legacy `wingfoil-python/examples/` scripts that had
no wingfoil twin, ported to `crates/wingfoil-python/examples/` (`combine.py`
— legacy's `examples.py` — `deduplicate.py`, `delay_line.py`, `latency.py`,
`kdb.py`, `iceoryx2_pubsub.py`, and the two `zmq/{direct,etcd}` pairs), and the
legacy `benches/README.md` reading inlined into the wingfoil benches README.

**The legacy copies stay until step 1.** The `legacy-kdb-integration.yml` and
`legacy-prometheus-integration.yml` twins still build from them, and they retire
with the tree — do not delete the legacy originals ahead of their workflows.

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

- **21 workflow lines** (15 files) and **81 docs/skills files**, as of
  `a6aae88`: `--manifest-path crates/wingfoil/Cargo.toml` → `-p wingfoil`.
  Re-count before starting; these drift with every merge.
- Lines referencing `--manifest-path legacy/...` — these go with the
  workflows that own them (step 4) or are docs that die with the tree.
- Restore `-p wingfoil-python` where it was rewritten to a manifest path.
- **`.cargo/config.toml`**: delete the `lint-legacy` and `test-legacy` aliases
  and the `legacy/`-is-its-own-workspace note above them. Easy to miss — the
  aliases still resolve after the deletion, they just fail on a missing
  manifest.

Take the same care the rename needed: the pattern is `-p wingfoil(?![-\w])`.
A bare `-p wingfoil\b` **also matches `-p wingfoil-python`**, because a hyphen
is a word boundary — that mistake cost a CI round during 1.2.

## Step 4 — retire the legacy workflow set (5.2)

The collapse already happened, ahead of this runbook: the wingfoil workflows
own the plain filenames and every legacy twin carries a `legacy-` prefix. All
that is left here is deletion.

Delete these thirteen:

`legacy-adapter-integration.yml`, `legacy-aeron-integration.yml`,
`legacy-augurs-integration.yml`, `legacy-etcd-integration.yml`,
`legacy-iceoryx2-integration.yml`, `legacy-kafka-python-integration.yml`,
`legacy-kdb-integration.yml`, `legacy-otlp-integration.yml`,
`legacy-postgres-integration.yml`, `legacy-prometheus-integration.yml`,
`legacy-python-test.yml`, `legacy-redis-integration.yml`,
`legacy-zmq-etcd-integration.yml`.

Then drop their `legacy-*` job entries from `integration-tests.yml`, drop the
`legacy-python-test` job from `all-tests.yml`, and drop the `test-legacy` and
`lint-legacy` jobs from `rust-test.yml`.

**`legacy-augurs-integration.yml` has no wingfoil twin by design** — wingfoil's
augurs tests run inside `rust-test.yml` under `--all-features`. Retire it; there
is nothing to fold into.

✅ **The trading-e2e workflows are already repointed** (#776).
`build-trading-e2e-images.yml`, `build-trading-e2e-ami.yml` and
`deploy-trading-e2e.yml` build from
`crates/wingfoil/examples/showcase/trading_e2e/`; nothing is owed here.

**Five surviving workflows still name `legacy/` and need editing, not
deleting** — a `legacy-*` filename is not the only way a workflow depends on
the tree:

| File | What |
|---|---|
| `zmq-integration.yml` | two `legacy/wingfoil/**` path filters (the wire contract has two sides), and the whole *cross-engine* test step — it retires with `zmq_cross_engine_integration.rs` in step 2 |
| `release.yml` | the `legacy-zmq-integration` job — it builds the legacy workspace (`workspaces: legacy`) inside an otherwise-surviving workflow |
| `bump.yml` | the "Re-pin legacy's cross-workspace dependency" step, and `legacy/wingfoil/examples/latency_e2e/static/index.html` in its version-stamp list |
| `rust-fmt.yml` | the `cargo fmt --manifest-path legacy/Cargo.toml` leg |
| `rust-test.yml` | the `test-legacy` and `lint-legacy` jobs (already listed above) |

`crates-publish.yml` names legacy only in comments — including one explaining
that the parity dev-dependency carries no `version` and so never reaches the
registry. That comment describes a dependency step 2 deletes, so reword it
rather than leaving it describing something absent.

`scripts/disk.sh` also names `legacy/wingfoil-python/.venv` in its clean-up
list — harmless once gone, but it is the last reference outside CI.

Re-derive this list before starting rather than trusting it:
`grep -ln "legacy/" .github/workflows/*.yml | grep -v "/legacy-"`.

> ⚠️ **Check names change.** Deleting a workflow removes its CI check. If the
> repository has required status checks configured on `main`, they must be
> updated in the same window or merges will block on checks that can never
> report. This is the one step with a consequence outside the repo. (`next` is
> gone, so it is `main` alone now.)

## Step 5 — docs

- Root `CLAUDE.md`: remove the "Working under `legacy/`?" banner, the
  `legacy/`-is-its-own-workspace build section, and the `-p wingfoil`
  ambiguity note (step 3 removes the ambiguity itself).
- `docs/migration.md`: keep it. It is for users migrating *off* the legacy
  engine, and is more useful after the deletion, not less.
- `crates/README.md` and the architecture doc: drop the `legacy/` row and any
  "parity oracle" framing that is now historical.
- `docs/planning/port-plan.md` / `cutover-plan.md`: mark Phase 7 complete. Keep the
  rulings and the gate 6.4 numbers — that is the audit trail.
- **Prose that cites legacy paths as a source is fine and should stay** — the
  ported benches and examples say what they are ports *of*, and that lineage
  outlives the files. What must go is anything a reader or a tool would
  *follow*: markdown links into `legacy/`, and shell commands naming it.
- **Three service-name string literals** still spell `legacy/…` and are worth
  correcting while the tree is being cleaned:
  `examples/showcase/latency/shared.rs`'s `SERVICE_NAME`, and the
  `"legacy/wingfoil/examples/counter"` in the iceoryx2 example's `pub.rs` /
  `sub.rs` (plus its README). These are names a binary *emits*, so they are
  free to move — but pub and sub must change in the same commit, and any
  running peer has to be restarted.

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

## Step 7 — the swap itself ✅ **done**

Not in the plan's §5/§6 lists, and easy to forget. It has since happened, and
this step is kept as the record:

1. ✅ **The `next` → `main` PR landed.** `main` is now the trunk for the whole
   repository — the wingfoil tree at the root and `legacy/` alike. There is no
   longer a second integration branch.
2. ✅ **The branch filters are stripped** (2026-08-12). `next` was deleted from
   the remote, so the `[main, next]` `push`/`pull_request` filters and the
   `refs/heads/next` cache `save-if` guards were inert; they are gone from
   `rust-test.yml`, `python-test.yml` and `security-audit.yml`. (The plan named
   `all-tests.yml` and `rust-fmt.yml`; those carry no branch filter, so the
   residual of 5.6 was in the three above instead.)
3. ✅ **`next` is retired, not renamed.** `CLAUDE.md`'s branching section
   describes the single-trunk world: cut a feature branch from `main`, PR back
   into `main`, for every part of the tree including `legacy/`.

The `legacy/*` integration workflows still name `next` in their own filters.
They retire wholesale with the tree at step 4, so they are deliberately left
alone rather than edited twice.

## Step 8 — issues

**The re-labelling is already done** — all open issues carry `next`, and
nothing is left under `classic`. **#367 has since been closed**, so what this
step owes is the re-check alone (28 open as of 2026-08-12):

- Re-check the survivors against the deleted tree: **#450** wheels, **#452**
  dependabot, **#449 / #451 / #359** CI, **#461** supply chain, **#457**
  wingfoil-js, **#437** web historical streaming. All describe the surviving
  engine or its packaging, so they stay open — but **#437** in particular
  should be confirmed against wingfoil's web adapter rather than assumed to carry
  over, and the CI issues (#449 / #451) are partly answered by step 4's
  workflow collapse.
- Anything that still describes the *deleted* engine can be closed with a note
  pointing at this runbook.

## Order

Step 0 landed on its own, and had to: it is purely *additive* — copies out and
repoints, breaking nothing — so it could go in ahead of the tree being quiet,
and it is what makes steps 1–4 safe to run together.

Steps 1–4 are then one PR: each breaks the tree on its own, and only the
combination compiles. 5 can ride along. 6 gates it. 7 is done. 8 is independent
and can happen any time.
