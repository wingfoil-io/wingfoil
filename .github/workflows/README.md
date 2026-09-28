# GitHub Workflows

## CI (run on push / PR)

* `rust-test.yml` — three parallel jobs: `Coverage (unit)`, `Test (wingfoil)`
  and `Lint (fmt & clippy)`. The coverage leg measures the current engine with
  service-backed integration binaries excluded; those binaries are measured
  by their dedicated workflows below.
* `python-test.yml` — Python (`wingfoil-python`) build + pytest with coverage.
* `security-audit.yml` — fails on dependencies with known advisories
  (`cargo audit` for Cargo, `pnpm audit` for `wingfoil-js`, and
  `dependency-review` to block newly introduced vulnerable deps on PRs).
  Also runs weekly to catch advisories disclosed against pinned deps. Its
  counterpart is Dependabot **security** updates (a repository setting, not a
  `dependabot.yml` entry), which open the upgrade PRs — this workflow is the
  gate, Dependabot is the fix. Dependabot **version** updates are deliberately
  off; see [`../../SECURITY.md`](../../SECURITY.md) for why.
* `rust-fmt.yml` — `cargo fmt` check (manual dispatch).

**One push is exempt from the heavy legs.** `release.bump` pushes a commit
whose message is `bump: <type> version to <x.y.z>` and whose diff is version
strings and nothing else. The jobs in `rust-test.yml`, `python-test.yml` and
`web-integration.yml` carry a job-level `if` that skips exactly that shape, on
`push` only — `pull_request` and `workflow_call` have no `head_commit`, so the
guard is true there and every leg runs as before. It is deliberately a
commit-message guard and not a `paths-ignore`: the note at the top of
`rust-test.yml` records why `paths-ignore` was removed from that file and is
not coming back, and the paths a bump touches (`**/Cargo.toml` chief among
them) are ones that must otherwise always trigger CI. `security-audit.yml` is
not exempt — it is cheap and it is the one gate worth running unconditionally.

## Integration tests

`integration-tests.yml` is a meta workflow that fans out to the per-target
workflows below. `all-tests.yml` runs `rust-test.yml` + `python-test.yml` +
`integration-tests.yml`.

* `kdb-integration.yml` — KDB+ (custom Docker image, license secret).
* `etcd-integration.yml` — etcd (Docker container, Python tests).
* `kafka-integration.yml` — Kafka via Redpanda + Python tests.
* `redis-integration.yml` — redis (Docker container, Python tests).
* `postgres-integration.yml` — postgres (Docker container, Python tests).
* `prometheus-integration.yml` — Prometheus + Grafana stack via compose.
* `otlp-integration.yml` — OpenTelemetry collector + Python tests.
* `zmq-integration.yml` — ZMQ core pub/sub, etcd discovery and cross-language
  tests.
* `fix-integration.yml` — FIX same-process round trips + Python loopback.
* `fluvio-integration.yml` — Fluvio cluster via Docker + Python tests.
* `iceoryx2-integration.yml` — iceoryx2 (Local + IPC) + Python tests.
* `aeron-integration.yml` — Aeron (media driver via testcontainers, `aeron:ipc`).
* `web-integration.yml` — web adapter round trips (plain + TLS) + Python
  WebSocket tests, plus the browser half: the `wingfoil-wasm` codec build and
  the `js/` (`@wingfoil/client`) typecheck, build and `vitest` suite. Both
  halves speak the same `wingfoil-wire-types` contract, so they share a
  trigger. This is the only place `js/tests/` runs on push/PR; `pnpm test` runs
  again as a preflight inside `npm-publish.yml`.

The per-adapter integration workflows above are the *only* place their
`tests/*_integration.rs` binaries are executed. `rust-test.yml` compiles them
(so they stay type- and link-checked) but filters them out of its test run:
without the service each one needs, they only exercise connection-timeout
paths, and they are slow doing it.

**And the split runs the other way too: each adapter workflow names its own
binary with `--test` and runs nothing else.** Without a target selector,
`cargo test --features <adapter>-integration-test -p wingfoil` is the whole
package — the shared suite `rust-test.yml` already ran on the same commit. Ten
of the thirteen had drifted that way, at two to three minutes apiece.
`web-integration.yml` is the one deliberate overlap, and says so at the step.

Per-adapter Codecov flags dropped when this landed: they had been measuring
the shared suite, which the `unit` flag already counts. The project total is
unchanged.

**Coverage on these runs is post-merge only**, the same rule
`rust-test.yml`'s `Coverage (unit)` leg follows: instrumentation costs ~7.6x
on test execution, which no cache warming touches, and nothing gates on the
result. On a push to `main` each workflow instruments its Rust tests and
uploads one LCOV report under a stable adapter flag; on a pull request it runs
the identical tests uninstrumented and uploads nothing. The two paths differ
only by the `WF_CARGO_TEST` prefix, since `cargo llvm-cov --no-report` and
`cargo test` take identical argument tails.

`codecov.yml` sets `carryforward: true`, so a flag keeps its last complete
report on pull requests and when a narrower change does not trigger its
workflow. Export and upload are skipped when a test step fails, so a half-run
report never becomes that flag's baseline. Codecov project and patch statuses
remain informational while the current-engine baseline settles.

**Triggers: `push` to `main` plus `pull_request`, both path-filtered**, and
each lists its own filename among those paths so a change to the workflow
exercises it.

The `pull_request` half is what makes them a pre-merge gate, and it is new.
These were `push`-triggered with no branch filter and no `pull_request` trigger
at all, which meant they never ran on a contributor's PR: contributors work
from forks, and a push to a fork does not trigger this repository's workflows.
Since these are the only place the `*_integration.rs` binaries execute, an
adapter change was type- and link-checked and nothing more until it was already
on `main`.

The branch filter is the other half. A `push:` with no `branches:` ran on every
branch pushed here, and such a run executes the workflow file *from the pushed
branch* with repository secrets in scope — so anyone who could push a branch
could read any secret a push-triggered workflow touches, with no review in the
way. These should be reachable from `main`, a PR, `workflow_call` or a
dispatch, and not from an arbitrary branch push.

`kdb-integration.yml` is the single exception and stays post-merge only: it
needs `KDB_LICENSE_B64` to build its image, and a `pull_request` run from a
fork gets no secrets, so mirroring it would fail every external PR while
passing internal ones.

Every push/PR workflow declares a `concurrency` group so a superseded PR push
cancels its predecessor. The group name is a literal per file rather than
`${{ github.workflow }}` — under `workflow_call` that expression resolves to
the *caller's* workflow name, which would put every fanned-out leg of
`integration-tests.yml` in one group where they cancel each other.

## Release & publish (manual dispatch)

Two dispatches, in this order:

1. `bump.yml` — bump version (major/minor/patch). Pushes the version-string
   commit straight to `main`.
2. `release.yml` — everything else, in one run.

`release.yml` drives all three publish paths. `pypi-publish.yml` builds the
Python distributions, then an upload job defined in `release.yml` publishes
them with attestations. A direct dispatch of `pypi-publish.yml` remains the
TestPyPI rehearsal and dry-run path. **`npm-publish.yml` cannot be dispatched
directly** because npm matches its trusted publisher against the workflow a run
entered through. Recover a production publish through the `registries` input
below.

Its order is **publish first, tag last**:

```
preflight ─> all tests ─┬─> crates.io ─┐
                        ├─> npm        ├─> tag ─> GitHub release
                        └─> PyPI       ┘
```

The tag used to come *before* the three registry publishes, which made a publish
failure unrecoverable: the tag was already pushed, so the preflight's
`Fail if tag already exists` check blocked every re-run until someone deleted
the tag by hand. Nothing consumed the tag — each publish job checks out the
dispatched commit or artifacts built from that commit, not the tag — so moving
it after them costs nothing and means a failed release leaves no trace to clean
up. `github-release` still comes last of all, because
`gh release create --verify-tag` needs the tag, and because an announcement
pointing at versions nobody can install is worse than no announcement.

### Recovering a partial publish

A *partial* publish is the one state the publish-then-tag ordering does not
make idempotent on its own: if crates.io succeeds and npm fails, a plain
re-dispatch re-runs the crates job, which dies on "crate version already
uploaded".

The `registries` input is the way out. Dispatch `release.yml` again with it set
to the registry that still needs publishing (`crates`, `npm` or `pypi`) and the
other two registry paths skip. Preflight and the full test suite still run, and
the tag and GitHub release are still cut at the end — the tag was never pushed
by the failed run, so the recovery run is what completes the release.

`tag` is gated on "no publish job *failed*" rather than on `needs` alone, since
a skipped job would otherwise skip the tag with it. It keeps `all-tests` in its
`needs` so that a skip caused by a red suite is not mistaken for a skip the
dispatcher asked for.

### PyPI trusted publishing — what has to exist on PyPI

The upload authenticates with OIDC, so there is no `*_PYPI_API_TOKEN` secret
any more. 8.x published with one; the rewrite replaced it, which means the
first release to *use* trusted publishing is the one that discovers whether it
was ever set up. 9.0.0's upload failed with:

```
invalid-publisher: valid token, but no corresponding publisher
```

That is a PyPI-side configuration gap, not a workflow bug — nothing in this
repo can fix it. Production and test use separate indexes and entry-point
workflows, so each needs its own GitHub publisher:

| Index | Owner | Repository | Workflow name | Environment |
|---|---|---|---|---|
| PyPI | `wingfoil-io` | `wingfoil` | `release.yml` | *(blank)* |
| TestPyPI | `wingfoil-io` | `wingfoil` | `pypi-publish.yml` | *(blank)* |

PyPI looks a publisher up by the workflow filename in the
**`job_workflow_ref`** claim and lists `workflow_ref`, which names the caller,
among the claims it does not check
([warehouse `GitHubPublisher.lookup_by_claims`][lookup]). Sigstore attestations
identify the caller instead. Defining the production upload in `release.yml`
makes both identities agree, so the trusted-publishing action can emit PEP 740
attestations. The direct TestPyPI upload also has one identity because
`pypi-publish.yml` is its entry point.

[lookup]: https://github.com/pypi/warehouse/blob/main/warehouse/oidc/models/github.py

### Testing a change to how the wheels are built

The wheel jobs only ever ran at release time, which is how two build breaks
(a missing `rustfmt` component in the manylinux container, librdkafka's
`./configure` on MSVC) got to be discovered by a release rather than by CI.
Dispatch `pypi-publish.yml` with `pypi-target: dry-run` to build all five
wheels and the sdist and upload none of them — the artifacts are attached to
the run. `test` is not a substitute: TestPyPI takes a given version exactly
once too, so a build fix that needs two attempts has nowhere to land the
second.

`crates-publish.yml` publishes six crates in dependency order, `wingfoil-wasm`
among them — it is excluded from the root workspace, so it never appears in a
`--workspace` build and was missed for several releases. Its verification build
runs against `wasm32-unknown-unknown`, which is why that target is installed in
the job.

## Latency E2E demo

* `build-trading-e2e-images.yml` — build & push Fargate images to ECR.
* `build-trading-e2e-ami.yml` — build EC2 Spot AMI.
* `deploy-trading-e2e.yml` — deploy demo stack.

## Misc

* `bulk-rebase.yml` — rebase all open branches onto `main` (manual).

## Pre-commit checks

These must pass:

```
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```
