# Execution Adapter Example (wingfoil)

The execution loop end to end: a passive quoting strategy trading against a
simulated venue, over a replayed order book. Strategy → order → fill →
position → strategy, closed on the ordinary engine with no kernel support.

The point is the *shape* of the graph, not the strategy. The strategy consumes
books and positions and emits orders; what sits on the other side of the order
stream — `sim_venue` here, a venue's own protocol live — is a wiring decision
the strategy never sees. That is the property the
[`execution`](../../../src/adapters/execution/) vocabulary exists to buy, and
it is the same move [`market`](../market/) makes on the data side.

> **The fill model is deliberately dishonest.** It fills at touch, with no
> queue position, no fees and no market impact, and it lies in the flattering
> direction. That is gate P0 of Project Venue
> (`docs/planning/proposals/trading-stack.md`): prove the loop closes first, on
> cycle semantics rather than fill realism. An honest model is gate P2. Do not
> read the PnL below as a result.

## Run

No prerequisites — the book feed is built in.

```sh
cargo run -p wingfoil --example execution_adapter --features execution-sim
```

## Code

`feedback` is what makes the cycle a DAG: the strategy's orders go *into* the
sink, and the venue reads them off the paired source one cycle later. So the
loop is wired inside out — the venue's input exists before the strategy that
feeds it.

```rust,ignore
let (orders_at_venue, order_sink) = g.feedback::<Burst<Order>>();

let book = g.replay_results(feed(&instrument)).order_book();
let fills = book.sim_venue(&orders_at_venue);
let position = fills.position();

let orders = book.wire(move |b, h| b.quoter(h, position_handle, cfg));
let sent = orders.feedback(&order_sink);
```

That one-cycle delay is not a detail of this example — it is why a strategy
**cannot** observe a fill at the instant it ordered, whatever the simulator
does. The engine makes the causality violation unrepresentable rather than
leaving it to the venue's discipline.

The strategy is an ordinary op over two edges, each with its tick flag. The
position flag is how it learns its order filled:

```rust,ignore
#[op(build = quoter)]
impl Op for Quoter {
    type Cfg = QuoterCfg;
    type State = QuoterState;
    type In<'a> = (&'a Arc<OrderBook>, bool, &'a Position, bool);
    type Out = Burst<Order>;
    const ACTIVATION: Activation = Activation::NONE;

    fn cycle(..) -> Result<Tick<Burst<Order>>> {
        let (book, _book_ticked, position, position_ticked) = input;
        if position_ticked {
            state.working = false;
        }
        if state.working {
            return Ok(Tick::Quiet);
        }
        let (side, level) = if position.is_flat() {
            (Side::Bid, book.best_bid())
        } else {
            (Side::Ask, book.best_ask())
        };
        // … quote one unit at that level
    }
}
```

## Output

The book walks down and back up, so the passive quote on each side gets reached
in turn:

```text
  0ns  quote  q-1 buy  1 @ 100
 20ns  FILL   q-1 buy  1 @ 100
 20ns  pos    net 1 @ 100  realized 0
 20ns  quote  q-2 sell 1 @ 101
 40ns  FILL   q-2 sell 1 @ 101
 40ns  pos    net 0 @ -  realized 1
 40ns  quote  q-3 buy  1 @ 100
```

Two things in that trace are worth reading closely.

**The buy quoted at 0ns fills at 20ns, not at 10ns** — even though the ask
reaches its price of 100 at 10ns. An order matches the book **as of the
previous instant**, never the update it arrived alongside. Matching the other
way would let a strategy react to an update and fill against that same update
at the same instant: lookahead, dressed up as speed, and every passive strategy
would look better than it is. `tests/execution_loop.rs` pins this.

**At 20ns the fill, the position and the next quote all land in one cycle**, in
that order. That is topological order through the loop, not a coincidence of
scheduling — the fill is upstream of the position, which is upstream of the
strategy. The *next* order then reaches the venue at 21ns.

## Elsewhere

- [`market`](../market/) — the data-side half of the same story: one strategy,
  two venues, both run modes.
- [`../../../src/adapters/execution/`](../../../src/adapters/execution/) — the
  vocabulary, the position fold and the simulator, with the decisions the fill
  model makes explicitly.
