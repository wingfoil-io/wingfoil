# Trading Examples

The [`wingfoil::trading`](../../src/trading/mod.rs) layer: the venue-neutral
market data vocabulary and the execution layer built on it. Neither connects to
anything itself — a venue integration normalises into `trading::market` and
implements `trading::execution::venue::Venue` — so these run offline.
Each has its own README with prerequisites, the wiring, and expected output.

| Example | Feature | Run | What it does |
|---|---|---|---|
| [`market`](market/) | `market,fix,kdb` | `--example market_adapter` | One strategy over the venue-neutral book vocabulary, fed by either impl of its feed trait: LMAX FIX (realtime) or a kdb+ replay (historical). |
| [`execution`](execution/) | `execution` | `--example execution_order_edge` (and eight more) | The order edge, OMS, position fold, reconciler, kill switch and FIX bridge, on instruments of your own — offline, no venue. The graph examples against `testing::SimVenue` also need `execution-testing`. |

Target names predate this group and are unchanged: `market_adapter` and the
`execution_*` targets run exactly as they did from `adapters/`.
