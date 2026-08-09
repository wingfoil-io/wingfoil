# GitHub Workflows

## CI (run on push / PR)

* `rust-test.yml` — four parallel jobs: `Test (legacy) & Coverage`,
  `Test (wingfoil)`, `Lint (fmt & clippy)` and `Lint legacy (fmt & clippy)`.
  They were one serial job until they were split; the legs share no build
  artifacts (coverage builds into `target/llvm-cov-target` under
  `-C instrument-coverage`, the wingfoil tests build a third feature set), so
  serialising them bought nothing.
* `python-test.yml` — Python (`wingfoil-python`) build + pytest with coverage.
* `legacy-python-test.yml` — the same for the legacy bindings. Retires with
  `legacy/`.
* `security-audit.yml` — fails on dependencies with known advisories
  (`cargo audit` for Cargo, `pnpm audit` for `wingfoil-js`, and
  `dependency-review` to block newly introduced vulnerable deps on PRs).
  Also runs weekly to catch advisories disclosed against pinned deps. Its
  counterpart is Dependabot **security** updates (a repository setting, not a
  `dependabot.yml` entry), which open the upgrade PRs — this workflow is the
  gate, Dependabot is the fix. Dependabot **version** updates are deliberately
  off; see [`../../SECURITY.md`](../../SECURITY.md) for why.
* `rust-fmt.yml` — `cargo fmt` check (manual dispatch).

## Integration tests

`integration-tests.yml` is a meta workflow that fans out to the per-target
workflows below. `all-tests.yml` runs `rust-test.yml` + `python-test.yml` +
`legacy-python-test.yml` + `integration-tests.yml`.

Every adapter that exists in both trees has two workflows: the plain name
covers `crates/wingfoil`, and a `legacy-` prefixed twin covers `legacy/`. The
whole `legacy-*` set retires with the legacy tree.

* `kdb-integration.yml` — KDB+ (custom Docker image, license secret).
* `etcd-integration.yml` — etcd (Docker container, Python tests).
* `kafka-integration.yml` — Kafka via Redpanda + Python tests.
* `redis-integration.yml` — redis (Docker container, Python tests).
* `postgres-integration.yml` — postgres (Docker container, Python tests).
* `prometheus-integration.yml` — Prometheus + Grafana stack via compose.
* `otlp-integration.yml` — OpenTelemetry collector + Python tests.
* `zmq-integration.yml` — ZMQ core pub/sub, etcd discovery, cross-engine
  wire-compatibility and cross-language tests.
* `fix-integration.yml` — FIX same-process round trips + Python loopback.
* `fluvio-integration.yml` — Fluvio cluster via Docker + Python tests.
* `iceoryx2-integration.yml` — iceoryx2 (Local + IPC) + Python tests.
* `aeron-integration.yml` — Aeron (media driver via testcontainers, `aeron:ipc`).
* `web-integration.yml` — web adapter round trips (plain + TLS) + Python
  WebSocket tests, plus the browser half: the `wingfoil-wasm` codec build and
  the `js/` (`@wingfoil/client`) typecheck. Both halves speak the same
  `wingfoil-wire-types` contract, so they share a trigger.
* `legacy-adapter-integration.yml` — matrix: fix, fluvio, kafka, zmq.
  Pure-Rust legacy adapter integration tests sharing the same shape.
* `legacy-augurs-integration.yml` — augurs forecasting + Python tests.
* `legacy-kafka-python-integration.yml` — Kafka via Redpanda service container.
* `legacy-zmq-etcd-integration.yml` — ZMQ + etcd Python tests.
* `legacy-{kdb,etcd,redis,postgres,prometheus,otlp,iceoryx2,aeron}-integration.yml`
  — the legacy twins of the workflows above.

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

Run in this order, waiting for each to succeed:

1. `bump.yml` — bump version (major/minor/patch).
2. `release.yml` — preflight + run all tests + cut release tag.
3. `crates-publish.yml` — publish to crates.io.
4. `pypi-publish.yml` — publish to PyPI.
5. `npm-publish.yml` — publish to npm.

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
