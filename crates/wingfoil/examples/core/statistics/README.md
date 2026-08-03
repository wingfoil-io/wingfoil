## Streaming statistics

Rolling-window statistics and an exponentially-weighted moving average over a
single price stream, printed as a live table. This is the fluent-API port of the
legacy `statistics` example.

The numeric aggregations live in the `StatisticsOps` trait, kept *out* of the
prelude so the core combinator vocabulary stays small — bring it in explicitly:

```rust
use wingfoil::stats::StatisticsOps;

let price = g.ticker(Duration::from_millis(100)).count()
    .map(|n| 100.0 + ((*n as f64) * 0.6).sin() * 5.0);

let ewma = price.ewma_per_tick(0.3);
let sma  = price.rolling_mean(10);
let std  = price.rolling_std(10);
let lo   = price.rolling_min(10);
let hi   = price.rolling_max(10);
```

The columns are bundled into one row per tick with `join`, sampled twice a
second, and printed:

```text
  time   price    ewma     sma     std     min     max
  0.0s  102.82  102.82  102.82    0.00  102.82  102.82
  0.5s   97.79  101.29  102.37    2.70   97.79  104.87
  1.0s  101.56   99.00   99.84    3.73   95.02  104.87
  ...
```

### Scope and idiom

- **Bundling** — the legacy example used a `combine(columns)` op that packs an
  arbitrary number of columns into one `Stream<Burst<f64>>`. The next engine's
  ops are fixed-arity (join is two-input), so this port bundles the columns with
  a chain of `join`s building a `Vec<f64>` row. A variadic `combine`/`zip` op is
  a known gap.
- **Time-weighting** — the legacy example also showed a *time-weighted* mean
  (`Weighting::Time` / TWAP). The `StatisticsOps` trait now exposes the
  time-weighted moment family alongside the count-weighted ops —
  `{cumulative,rolling,time_windowed}_{mean,var,std}_time_weighted` (each sample
  weighted by how long it was in effect, read from the graph clock) — so a TWAP
  column is expressible; this example keeps the count-weighted columns for
  brevity. The only legacy statistic still unported is the time-weighted
  *median*.
