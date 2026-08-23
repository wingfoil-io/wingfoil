# GitHub Workflows

## CI (run on push / PR)

* `rust-test.yml` — two parallel jobs: `Test (wingfoil)` and
  `Lint (fmt & clippy)`. They were one serial job until they were split; the
  legs share no build artifacts, so serialising them bought nothing.
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
  trigger. Unlike the service-backed integration workflows above, this one
  also runs on pull requests because it needs no license secret or external
  service. This is the only place `js/tests/` runs on push/PR; `pnpm test` runs
  again as a preflight inside `npm-publish.yml`.

The per-adapter integration workflows above are the *only* place their
`tests/*_integration.rs` binaries are executed. `rust-test.yml` compiles them
(so they stay type- and link-checked) but filters them out of its test run:
without the service each one needs, they only exercise connection-timeout
paths, and they are slow doing it.

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

`release.yml` drives the three publish workflows itself. `crates-publish.yml`
and `pypi-publish.yml` can be dispatched by hand for recovery;
**`npm-publish.yml` cannot**, and is `workflow_call`-only for that reason — npm
matches its trusted publisher against the workflow a run *entered* through, so
only one of the two entry points can ever authenticate. `release.yml` is the
registered one. Recover npm through the `registries` input below instead.

Its order is **publish first, tag last**:

```
preflight ─> all tests ─┬─> crates.io ─┐
                        ├─> npm        ├─> tag ─> GitHub release
                        └─> PyPI       ┘
```

The tag used to come *before* the three publishes, which made a publish
failure unrecoverable: the tag was already pushed, so the preflight's
`Fail if tag already exists` check blocked every re-run until someone deleted
the tag by hand. Nothing consumed the tag — each publish job checks out the
dispatched commit, not the tag — so moving it after them costs nothing and
means a failed release leaves no trace to clean up. `github-release` still
comes last of all, because `gh release create --verify-tag` needs the tag, and
because an announcement pointing at versions nobody can install is worse than
no announcement.

### Recovering a partial publish

A *partial* publish is the one state the publish-then-tag ordering does not
make idempotent on its own: if crates.io succeeds and npm fails, a plain
re-dispatch re-runs the crates job, which dies on "crate version already
uploaded".

The `registries` input is the way out. Dispatch `release.yml` again with it set
to the registry that still needs publishing (`crates`, `npm` or `pypi`) and the
other two publish jobs skip. Preflight and the full test suite still run, and
the tag and GitHub release are still cut at the end — the tag was never pushed
by the failed run, so the recovery run is what completes the release.

`tag` is gated on "no publish job *failed*" rather than on `needs` alone, since
a skipped job would otherwise skip the tag with it. It keeps `all-tests` in its
`needs` so that a skip caused by a red suite is not mistaken for a skip the
dispatcher asked for.

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
