# GitHub Workflows

## CI (run on push / PR)

* `rust-test.yml` — Rust build, test, clippy, coverage upload.
* `python-test.yml` — Python (`wingfoil-python`) build + pytest with coverage.
* `rust-fmt.yml` — `cargo fmt` check (manual dispatch).

## Integration tests

`integration-tests.yml` is a meta workflow that fans out to the per-target
workflows below. `all-tests.yml` runs `rust-test.yml` + `python-test.yml` +
`integration-tests.yml`.

* `adapter-integration.yml` — matrix: aeron, fix, fluvio, kafka, zmq.
  Pure-Rust adapter integration tests sharing the same shape.
* `kdb-integration.yml` — KDB+ (custom Docker image, license secret).
* `etcd-integration.yml` — etcd (Docker container, Python tests).
* `prometheus-integration.yml` — Prometheus + Grafana stack via compose.
* `otlp-integration.yml` — OpenTelemetry collector + Python tests.
* `iceoryx2-integration.yml` — iceoryx2 (Local + IPC) + Python tests.
* `kafka-python-integration.yml` — Kafka via Redpanda service container.
* `zmq-etcd-integration.yml` — ZMQ + etcd Python tests.
* `web-integration.yml` — `wingfoil-wasm` build + `wingfoil-js` typecheck.

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
