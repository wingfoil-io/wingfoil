# statistics Adapter (wingfoil)

EWMA and rolling-window statistics over an `f64` stream. Ports legacy
`wingfoil::adapters::statistics` onto the Op model — **same module path, same
trait name**, so a migrating user changes an import only if they were on the
wingfoil-side `stats` path that briefly existed.

**A pure-compute adapter — no I/O edge, no service, no dependency.** The same
shape as [`augurs`](../augurs/CLAUDE.md), with a lighter kernel: transform ops
plus one extension trait.

## Layout

```
adapters/
  statistics.rs          # StatisticsOps — the fluent trait, 36 methods
  statistics/CLAUDE.md   # this file
```

The **ops themselves are not here**. `Ewma`, `RollingMoment`, `RollingExtreme`,
`Window`-family and friends live in `crates/wingfoil/src/ops.rs` alongside the
rest of the catalog, and `statistics.rs` is the trait that gives them fluent
names via the `__wf_fluent_*!` macros `#[op(fluent)]` generates. That split is
deliberate and predates the move: the compiled tier and `nitro!` reach the ops
directly, so putting them behind a feature would gate a third of the catalog.

## Feature gating

```toml
statistics = []
```

**Empty on purpose** — the statistics are hand-rolled, exactly as legacy's
were. The gate controls the *surface*, not a build cost, which puts it in the
same class as `market = []`. Legacy shipped this module ungated; gating is what
makes it consistent with every other adapter.

Two consequences to know:

1. **`nitro!` does not glob it.** The macro's generated module imports
   `fluent::*` and `ops::*`, and deliberately no feature-gated adapter trait —
   a path emitted for a feature the user has not enabled would not resolve. So
   a statistics op inside a `nitro!` block needs
   `use wingfoil::adapters::statistics::StatisticsOps;` in the surrounding
   file, exactly as it does outside one. `tests/macro_parity.rs` is the
   in-tree example.
2. **`adapters` is declared after `ops` in `lib.rs`.** The `__wf_fluent_*!`
   macros are `macro_rules!` emitted by a proc macro, and those resolve by
   *textual scope* — i.e. module declaration order (rustc #52234). This module
   is the only reason the whole adapter tree sits below `ops`; the comment in
   `lib.rs` says so.

## Tests

Six files, all `#![cfg(feature = "statistics")]`:

| File | Covers |
|---|---|
| `tests/statistics.rs` | the original port's tractability proof |
| `tests/statistics_rolling.rs` | count-windowed rolling family |
| `tests/statistics_cumulative.rs` | unbounded-window cumulative family |
| `tests/statistics_time_windowed.rs` | time-windowed family |
| `tests/statistics_time_weighted.rs` | time-weighted moments |
| `tests/statistics_time_weighted_median.rs` | time-weighted median |

Plus the gated items inside `tests/op_completeness.rs` (the four
`surface_stats_*` `nitro!` blocks putting all 36 methods under the two-sided
guard), `tests/island_scheduling.rs` (a statistics op inside an island) and
`tests/macro_parity.rs`.

No tier-2 integration test — there is no service to stand up.

## Python

`crates/wingfoil-python/src/statistics.rs` binds the whole surface, dispatching
legacy's two orthogonal knobs (`Window` × `Weighting`) onto this trait's
one-method-per-combination shape. The binding's `wingfoil` dependency enables
`statistics` **non-optionally**: a wheel is the only copy a Python user gets,
so an adapter left out of it is simply absent.
