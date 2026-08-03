# augurs Adapter (wingfoil)

On-graph time-series analysis over the [`augurs`](https://docs.rs/augurs)
toolkit. Ports legacy `wingfoil::adapters::augurs` onto the Op model.

**A pure-compute adapter — no I/O edge, no service, no lifecycle.** It is
*transform ops*, the same shape as `stats`, just with a heavier kernel. That
makes it the reference for skill step 9 (custom `Op`s + `#[op(build = …)]`).

## Layout

```
adapters/
  augurs.rs          # six ops: config types, Cfg resolvers, Op impls, extension traits
  augurs/CLAUDE.md   # this file
```

## Feature gating

```toml
augurs = ["dep:augurs"]
```

No `async`, no integration-test feature, no testcontainers. The `augurs`
dependency is pinned `default-features = false` with **exactly** legacy's
sub-feature set, one per ported op:
`ets, mstl, outlier, changepoint, seasons, dtw, clustering`. Prophet is
deliberately excluded (it needs a bundled Stan toolchain) — as in legacy.

If you add an op needing another sub-feature, widen that list *and* the comment
above the dep (skill step 13).

## Entry points — all six of legacy's operators

Each is an extension trait; bring in the ones you use.

| Trait method | Input | Output | Model |
|---|---|---|---|
| `AugursForecastOps::augurs_forecast` | `Stream<f64>` | `AugursForecast` | AutoETS or MSTL |
| `AugursOutlierOps::augurs_outlier` | `Stream<Vec<f64>>` | `AugursOutliers` | MAD or DBSCAN |
| `AugursChangepointOps::augurs_changepoint` | `Stream<f64>` | `AugursChangepoints` | BOCPD |
| `AugursSeasonsOps::augurs_seasons` | `Stream<f64>` | `AugursSeasons` | periodogram |
| `AugursDtwOps::augurs_dtw` | `Stream<Vec<f64>>` | `AugursDistanceMatrix` | DTW |
| `AugursClusterOps::augurs_cluster` | `Stream<Vec<f64>>` | `AugursClusters` | DBSCAN over DTW |

Each takes `impl Into<…Config>`; the configs have `From` impls for the common
tuple shapes (e.g. `From<(usize, usize)>` for `AugursForecastConfig`), so one
signature serves several call sites.

## What to know before changing it

- **The `Op` shape.** `Cfg` = the *resolved* config (validated/floored at
  wiring into a `…Cfg`), `State` = the sliding window (`VecDeque`, `Default`),
  `In<'a> = (&'a I,)`, `ACTIVATION = Activation::NONE`.
  `#[op(build = augurs_forecast)]` generates both the interpreted
  `Builder::augurs_forecast` method and the forwarders that make the op usable
  inside `nitro!` / `compiled()` — there is no per-op macro table to edit.
- **Warm-up returns `Tick::Quiet`**, a full window returns `Tick::Value`.
- **Config errors are `anyhow` errors from inside `cycle`, never a panic at
  wiring.** Legacy's outlier construction panics on a bad sensitivity
  (`MADDetector::with_sensitivity(..).unwrap_or_else(|e| panic!(..))`); next
  builds the detector in `cycle` and bails. That is a deliberate improvement —
  do not "fix" it back.
- **Models refit every tick.** This is real CPU on the single-threaded engine:
  the ETS model search, a BOCPD re-scan of the window, DTW at
  `O(n² · window²)` for `n` series. The docs tell callers to `throttle`
  upstream; keep that guidance rather than adding caching heuristics.
- **`augurs_cluster` and `augurs_dtw` floor their effective window at 2.**
  Legacy's cluster node sizes its buffer for two samples but evicts against
  the raw `window`, so `window == 1` never warms up and never ticks. Next grows
  the effective window to the floor for both (register **D12**).
- Some augurs errors are not `Send + Sync`, so they cannot flow through
  `Context` — they are mapped with `map_err(|e| anyhow::anyhow!(…))`. Keep that
  pattern for new ops.

## Deviations from legacy

Canonical list: the `# Deviations from legacy` block in `augurs.rs` — the
fallible-config change and the `augurs_cluster` window floor, both above.
Capability-wise the port is **complete**: register **C5** (originally "only
`augurs_forecast` + `augurs_outlier` ported") is resolved, all six operators
land, and the sub-feature list was widened to match. Do not reintroduce a
"subset ported" note anywhere.

## Tests

| File | Gate | Needs |
|---|---|---|
| `tests/augurs_adapter.rs` | `#![cfg(feature = "augurs")]` | nothing |

Parity port of legacy's unit tests for all six operators (augurs models are
deterministic given their inputs).

```bash
cargo test --manifest-path crates/wingfoil/Cargo.toml --features augurs --test augurs_adapter
```

No integration tier and no `augurs-next-integration.yml` — there is nothing to
stand up (skill step 10, Option C). Runs in `rust-test.yml`'s `test-next` job.
Note `.github/workflows/augurs-integration.yml` exists but is the **legacy**
adapter's.

## Example

`examples/augurs_adapter.rs`, `required-features = ["augurs"]` — covers all six
operators.

## Python

`wingfoil-python` feature `augurs = ["wingfoil/augurs", "_common"]`.
**In `all-adapters` and in the wheel** (pure Rust compute, no I/O, nothing
platform-specific).

- Entry points, all `#[pyadapter]`, in `src/adapters/augurs.rs`:
  `augurs_forecast`, `augurs_changepoint`, `augurs_seasons`, `augurs_outlier`,
  `augurs_dtw`, `augurs_cluster` — one per Rust op.
- Tests: `tests/test_augurs.py`, **no marker**, runs by default in
  `next-python-test.yml`. No integration workflow.

## Pre-commit

```bash
cargo fmt --all
cargo lint
cargo lint-all
cargo test --manifest-path crates/wingfoil/Cargo.toml --features augurs
```
