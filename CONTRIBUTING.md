# Contributing to Wingfoil

We'd love your help. Say hi on [Discord](https://discord.gg/WfZwpQnZUA), open a
[discussion](https://github.com/wingfoil-io/wingfoil/discussions), or comment
on any issue you fancy.

## Getting set up

You need the Rust toolchain (latest stable, with `rustfmt` and `clippy`) and
`protoc` — a transitive dependency builds proto files, so a plain workspace
build needs it:

```bash
scripts/setup-dev.sh              # installs protoc; Debian/Ubuntu and macOS
```

Then check everything works end to end:

```bash
git clone https://github.com/wingfoil-io/wingfoil.git && cd wingfoil
cargo test -p wingfoil
cargo run  -p wingfoil --example hello_graph
```

A few adapters need more (Aeron wants clang, libuuid and CMake ≥ 3.30; some
adapter tests want a live service) — [`CLAUDE.md`](CLAUDE.md) has the details,
and none of it is needed to work on the engine.

**Where to start:** the
[`good first issue`](https://github.com/wingfoil-io/wingfoil/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
label, or anything labelled `size: small`. Issues also carry `priority:` and
area labels (`core`, `io-adapter`, `python`) if you want to browse by
interest. Not sure whether an idea fits? Ask first in an issue or on Discord —
that is cheaper for both of us than a PR that has to be unwound.

**Read first:** [`docs/wingfoil-architecture.md`](docs/wingfoil-architecture.md)
is the shape of the engine and the one decision everything else follows from.
Worth 20 minutes before your first non-trivial change.

## How the work is organised

Wingfoil is a ground-up rebuild, on the Op pattern, of the original
`MutableNode` engine — see [`README.md`](README.md) for the design objectives
and [`docs/planning/port-plan.md`](docs/planning/port-plan.md) for the
historical account of the port.

**Everything branches from and merges into `main`.** `next` was the integration
branch that staged the replacement engine; it has landed and is retired, so
there is no second base branch any more.

Never commit directly to `main`. Cut a branch, push it, open a PR with base
`main`. Branch names are simple and descriptive — `add-metrics`,
`fix-error-handling`.

## What contributions look like here

The most valuable contributions are:

- **A new node/operator** — follow the `/new-op` skill
  (`.claude/commands/new-op.md` from the repo root) and "Adding an op" in
  [`docs/adding-an-op.md`](docs/adding-an-op.md). Most single-input ops need
  only an `Op` impl with `#[op(build = ...)]` plus a 3-line fluent method; the
  compiled path is zero-touch.
- **A new I/O adapter** — follow the `/new-adapter` skill
  (`.claude/commands/new-adapter.md` from the repo root), which encodes
  the layering rules (sources over `channel`/`poll`, sinks over `for_each`,
  extension traits out of the prelude).
- **Python bindings for an adapter** — the `/bind-adapter` skill.

## Ground rules

1. **Behaviour is pinned, not re-derived.** Tests assert exact values *and*
   tick times. Where an expectation was captured from the original engine it is
   a constant with its provenance in a comment — do not weaken it to make a
   change pass.
2. **One mechanism per op.** Semantics live in one `Op::cycle` — no
   duplicated logic per engine, no per-op tables in the macro.
3. **Burst model.** Same-instant values are delivered atomically in one
   `Burst`; nothing is coalesced or dropped.
4. **Fallible, with context.** No `.unwrap()` outside `#[cfg(test)]` and doc
   examples; propagate with `?` and `anyhow::Context` at I/O boundaries.
5. **No locks on the graph path.** Background threads talk to the graph
   through the channel layer.

## Building and testing

From the repository root (the crates are root-workspace members):

```bash
cargo build -p wingfoil
cargo test  -p wingfoil --all-features
cargo bench -p wingfoil          # three-tier regression gate
cargo fmt --all
cargo lint && cargo lint-all          # workspace clippy aliases, mirror CI
```

The default feature set is empty (`default = []`) and dependency-free — every
adapter is behind its own feature. Run `cargo lint-all` before pushing:
feature-gated code is the easiest thing to break without noticing, and it is
what CI runs.

### Tests that need a live service

Adapter tests come in two files. `tests/<name>_adapter.rs` needs nothing
running and is part of the ordinary `cargo test` suite.
`tests/<name>_integration.rs` needs a real service or real sockets, and is
**compiled but not run** by the normal job — CI's `test` job filters it out
with `-E 'not binary(/_integration$/)'`, so the `_integration` filename suffix
is the only thing keeping it out. Each one runs in its own workflow instead
(see [`.github/workflows/README.md`](.github/workflows/README.md)).

To run one locally you need its feature *and* whatever it talks to. Every
`*_integration.rs` file opens with the exact command and prerequisites — read
that header first. The three shapes:

- **Docker, brought up by the test.** etcd, redis, postgres, kafka, fluvio,
  otlp and aeron use [`testcontainers`](https://crates.io/crates/testcontainers)
  and start their own container, so a running Docker daemon is the whole
  prerequisite:

  ```bash
  cargo test -p wingfoil \
    --features redis-integration-test -- --test-threads=1 --nocapture
  ```

  Without Docker these fail with `Socket not found: /var/run/docker.sock`.

- **Docker, brought up by you.** Prometheus scrapes a live exporter, so bring
  the stack up first (`docker compose -f
  crates/wingfoil/examples/adapters/telemetry/docker/docker-compose.yml up
  -d`);
  the test skips itself with a printed notice if Prometheus is unreachable.
  KDB+ has no freely-licensed image at all — start a `q -p 5000` yourself
  (`KDB_TEST_HOST` / `KDB_TEST_PORT` to point elsewhere), and it likewise
  skips rather than fails when there is nothing there.

- **No service at all**, just real sockets or shared memory: `web` (in-process
  server over loopback), `zmq` (needs `libzmq`), `fix` (in-process
  acceptor + initiator) and `iceoryx2` (a writable `/dev/shm`). These are
  tier-2 only because they are slow and timing-sensitive, not because they
  need infrastructure — the feature flag is all they want.

Because they run against a live wall clock, integration tests generally assert
*values* rather than exact tick times; the deterministic
`HistoricalFrom(NanoTime::ZERO)` value-and-tick-time assertions belong in the
`_adapter.rs` half.

## Releasing

Maintainers only, and both steps are manual dispatches — see
[`docs/RELEASING.md`](docs/RELEASING.md).
