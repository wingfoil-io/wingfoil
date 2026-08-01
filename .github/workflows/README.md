# GitHub Workflows

## CI (run on push / PR)

* `rust-test.yml` — three parallel jobs: `Test (wingfoil) & Coverage`,
  `Test (wingfoil-next)`, and `Lint (fmt & clippy)`. They were one serial job
  until they were split; the legs share no build artifacts (coverage builds
  into `target/llvm-cov-target` under `-C instrument-coverage`, the next-engine
  tests build a third feature set), so serialising them bought nothing.
* `python-test.yml` — Python (`wingfoil-python`) build + pytest with coverage.
* `security-audit.yml` — fails on dependencies with known advisories
  (`cargo audit` for Cargo, `pnpm audit` for `wingfoil-js`, and
  `dependency-review` to block newly introduced vulnerable deps on PRs).
  Also runs weekly to catch advisories disclosed against pinned deps.
* `rust-fmt.yml` — `cargo fmt` check (manual dispatch).

## Integration tests

`integration-tests.yml` is a meta workflow that fans out to the per-target
workflows below. `all-tests.yml` runs `rust-test.yml` + `python-test.yml` +
`integration-tests.yml`.

* `adapter-integration.yml` — matrix: fix, fluvio, kafka, zmq.
  Pure-Rust adapter integration tests sharing the same shape.
* `kdb-integration.yml` — KDB+ (custom Docker image, license secret).
* `etcd-integration.yml` — etcd (Docker container, Python tests).
* `prometheus-integration.yml` — Prometheus + Grafana stack via compose.
* `otlp-integration.yml` — OpenTelemetry collector + Python tests.
* `iceoryx2-integration.yml` — iceoryx2 (Local + IPC) + Python tests.
* `aeron-integration.yml` — Aeron (media driver via testcontainers, `aeron:ipc`).
* `kafka-python-integration.yml` — Kafka via Redpanda service container.
* `zmq-etcd-integration.yml` — ZMQ + etcd Python tests.
* `web-integration.yml` — `wingfoil-wasm` build + `wingfoil-js` typecheck.

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

* `build-latency-e2e-images.yml` — build & push Fargate images to ECR.
* `build-latency-e2e-ami.yml` — build EC2 Spot AMI.
* `deploy-latency-e2e.yml` — deploy demo stack.

## Misc

* `bulk-rebase.yml` — rebase all open branches onto `main` (manual).

## Pre-commit checks

These must pass:

```
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
```
