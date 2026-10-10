# execution adapter (wingfoil)

`src/adapters/execution/` (rooted at `mod.rs`), feature `execution`
(and `execution-testing` for `testing::FixVenue`). **No legacy twin.** Like
`market`, it connects to nothing: it is the venue- and asset-neutral execution
layer that venue integrations implement and strategies drive.

## Rules specific to this module

- **Nothing here knows a venue or an asset class.** It builds on `market` and
  the engine and nothing else. A venue's limits, fees or id format are
  constants in its integration, built with this module's constructors; a
  type that needs a contract spec, a greek or a venue's constant is not
  ready to live here — make it generic first (a type parameter, a trait, a
  value passed in). The prose follows the same rule: say "the venue", "this
  edge", "the base currency".
- **The instrument is a type parameter, never a bound on a trait of ours.**
  `Order<I>` and friends need `I: Copy` to stay plain values and
  `I: Default` for the `Burst` placeholder, and read nothing else of it.
  Anything that needs to *know* the instrument (a fee, a valuation, a size
  cap) takes it as a value or a closure from the caller.
- **`rate_limit::OrderRate` has no `Default`.** A venue's limits are its
  integration's constant, built with the `const` `OrderRate::new`; a venue
  that states none says so with `OrderRate::UNMETERED`, never with a number
  nobody stated. The one-line forms are
  `OrderRate::stated(Terms::per_second(..), Terms::per_second(..), headroom)`
  (a const panic, so a compile error, where `new` would refuse) and
  `oms::Config::unmetered().with_rate(VENUE_RATE)` — `Config` has no
  `Default` either, so the name says what was assumed until a rate is stated.
- **Errors are `Copy` enums a caller matches on**, with a hand-written
  `Display` and `std::error::Error` — no `thiserror`, so they convert into
  `anyhow::Error` with `?` like every other error in the crate. The message
  states the fact; the reasoning sits on the variant's doc comment.

## The vocabulary and its arithmetic
- **`Order` and `Fill` are `Burst<_>` on both edges.** Same-instant orders and
  fills ride one burst, and a fold applies the whole burst in order.
- **Keep the execution vocabulary `Copy`.** A `Fill` is a plain value on the
  edge: a burst of them is copied, queued and matched without the allocator,
  and a `Burst<Fill>` holds them inline. A single `String` field would take
  that away from the whole type — that is why `ExecId` exists. (It is not what
  the position fold keys on; that keys on the instrument.) Any new field
  either fits inline or has to justify itself.
- **Distinct `ClientOrderId`s keep otherwise-identical orders distinct
  values.** That is what defuses `TimeQueue` dedup on the feedback edge
  (Project Venue §5.2). Don't make ids derivable from order contents.
- **Ids carry the process's epoch.** A `ClientOrderId` is `Epoch` (18 bits)
  above a 32-bit sequence (42), so a restarted process never mints the last
  one's numbers. The epoch is handed in by the caller; the module never reads
  a clock itself. `Epoch::from_clock(secs)` is there for a caller with
  nothing persisted to step from — it wraps every ~3 days, which is fine
  because an id need only differ from the last process's. Keep every id
  below 2⁵¹ (exact as a JSON double): widening the epoch is a wire change,
  not a constant. `fix::ReplaceChain` mints its `ClOrdId`s under the same
  epoch, for the same reason.
- **Both timestamps, and `recv_time` is `Ctx::time`.** Never the wall clock.
- **Defaults must be inert.** `OrderKind` defaults to `PostOnly` because it is
  the only variant that cannot cross. If you add a variant, that stays true.
- **Parsing a venue id refuses what it cannot hold.** `ExecId` (a fill's
  execution) and `VenueId` (the venue's order, on an `Ack`) are distinct
  types over one inline representation, so neither stands in for the other.
  Never truncate either — raise the shared capacity (`ExecId::CAPACITY`)
  instead. A silent truncation surfaces at settlement.
- **Build an order through a constructor.** `Order::limit`, `Order::post_only`
  and `Order::market` cannot state a `kind`, `price` and `tif` that disagree,
  which is what leaves `Order::validate` a backstop rather than the only
  check.
- **Money arithmetic goes through `market`'s operators and `mul_div`** —
  price × quantity, notional, anything rescaled. Never spell the scale out at
  a call site, and multiply before dividing: `qty × SCALE / price` keeps nine
  significant digits where taking the reciprocal first keeps four. `raw()`
  is fine for comparing two values of one type, and `to_f64` for a ratio
  judged against a tolerance (a loss limit); neither for a value that is
  then carried on as money.

## The OMS

- **Read `oms`'s module docs before touching it.** Two invariants are the
  component: one request in flight per slot, and
  nothing emitted that matches what is already working. The edge carries a
  `Request` and a `Report` — `Order` cannot say cancel — and they are `Copy`
  and `Burst` for the same reason `Order` is.
- **A graph drives the OMS through `OmsOps`, and only through it.** The
  order its entry points are called in within an instant — reports, the
  trading state, a cancel-all (its request at the head of the burst), the
  desireds, then a diff on every cycle, the sweep's included — is
  `oms::OmsOp`'s, argued in `oms::node`'s module docs. A caller must not
  hold an `Oms` in a `RefCell` and call it by hand from a graph: that is
  how every caller came to re-derive the order. `oms_reading` hands a
  caller a look at the OMS after the diff, never a lever. The pure methods
  stay for tests and for anything that is not a graph.
- **A side is a `Ladder`, and slots are not ranked.** Up to `MAX_DEPTH`
  levels a side; the diff matches them to the
  slots by price, then by rank, as `oms`'s module docs and `Oms::diff`
  say. Never key anything on a slot's index: it names where a report
  routes, not where its order sits in the ladder. A ladder is built by its
  constructors, which refuse two levels at one price.
- **Build a `Desired` through the constructor for its shape** —
  `Desired::quote`, `two_way`, `rest`, `cross`, `stop`, `nothing` and
  `no_trigger`, then `.reduce_only()` — which take the `as_of` as a required
  argument, because it is what the OMS keys answers and staleness on, and
  cannot build a crossing or triggered ladder deeper than one.
- **A burst of desireds is not the whole book.** A key absent from one wants
  what it wanted last time; a quoter and a hedger decide on separate
  edges, and a burst carrying one must not cancel the other. What withdraws a
  key is `max_desired_age`, judged at the reader's own engine time — so a
  stalled graph withdraws rather than resting on a strategy that has stopped
  thinking.
- **The OMS emits in a stated order.** It walks its books to build a burst,
  and the books are a `BTreeMap`, so the walk is in the instrument's order —
  never a `HashMap`, whose iteration order is randomised per process. A replay
  that emitted requests in a different order than the run it replays is
  replay determinism, broken silently. The position `Book`
  and the reconciler are ordered maps for the same reason.
- **The OMS shows a passive level as `Config::passive` says — post-only,
  or a plain limit on a venue with none, whose price guard is then the
  strategy's — good-till-cancelled, unless it is told to cross.** `oms::Intent::Cross` on a desired is a *kind on the wanted side*,
  not a new state: it is sent as a limit, immediate-or-cancel, at the level's
  price as a cap, never as a market order, and it uses the ordinary slot. A
  cross is one-shot per decision (the level — the `as_of` and the price — is
  spent, as a refused place is), and two on one instrument are `Config::retake` apart,
  because where a report decides at once a killed IOC's report wakes a decision that would
  cross again the next instant. A resting order is cancelled before it is
  crossed, never amended into a cross.
- **Every request the OMS emits spends a token, and what waits is re-planned,
  never queued.** The budget, its priority and superseding are argued in
  `oms`'s module docs; `rate_limit::OrderRate` has no
  `Default`, and the OMS spends its headroom, never the stated terms. A
  venue's message-to-trade `Ratio` holds places and amends, **never a
  cancel**: a kill switch that could not pull its quotes because the book
  had not traded enough is the wrong way round.
- **A book that is not open takes cancels and nothing else.** The venue's
  `TradingState` reaches the OMS through `Oms::trading`; while it is not
  `Open`, places and amends are held — not spent, not deferred as unpaid —
  and go out on the first diff after the open if still wanted and fresh.
  A continuous venue never states one, and the OMS starts open. A day
  order's close arrives as `Expired`, which retires the slot like any
  other end.
- **A slot compares against what is still showing**, not what was sent. A
  partially filled quote is amended back up; comparing against the original
  size would leave it short for as long as the strategy kept wanting the same
  thing.
- **An ack says what rests, where the venue says.** `Ack::price` and
  `Ack::remaining` are the venue's accepted level and showing size — a
  price rounded to its tick, a size trimmed to its lot — and the slot works
  those, not the ask. An accept that differs from the ask answers the
  decision (`Oms::adjusted` counts them): the working order is left as the
  venue rests it, neither amended back to the ask — which the venue would
  round again every instant — nor pulled, until the strategy's next
  decision asks again. A venue that names neither leaves the OMS believing the
  ask, which is all it can do.
- **A post-only reject is ordinary, not an error.** The touch moved between
  the decision and the arrival; the slot returns to idle and the next tick
  re-decides. Its *rate* is a metric. Latching on a storm is risk's. "The next
  tick re-decides" means the **strategy** re-decides: the fold holds the
  refused level until a desired arrives with a newer `as_of` than the one the
  order was placed from (`Placed::as_of`), per level, because
  re-sending the level the venue just refused is the same question with the
  same answer — and in a graph where the rejection wakes the fold, it is
  an instant-by-instant loop. **A refused amend is held the same way**, by
  the other mechanism: the level it asked for is marked answered by the
  order that still rests (as an ack at the venue's own tick is), never
  spent — a spent level is unwanted, and the resting order would be pulled
  for it.

## Parent orders

- **`algo` is a layer above the OMS and the OMS does not change for it.**
  A parent emits `Desired`s and reads fills; no request kind, slot state or
  tag on `Order` / `Fill` is added for it. Its rules — keyed on the
  instrument, a cross restated at its own `as_of`, an iceberg restated
  every beat, engine time throughout — are argued in `algo`'s module docs;
  a graph drives it through `AlgoOps`, in the order `algo::node` states.
- **Fill `Desired` in one place.** `Working::showing` is the only site that
  builds a parent's desired, so a change to `Desired`'s shape is one edit.

## Triggers

- **A triggered desired is a decision of its own on the instrument.**
  `Intent::Trigger` is remembered beside the plain desired, never over it,
  so a stop and the orders working its leg cannot withdraw each other; one
  level, one side. A fired order is never amended back under a trigger,
  and is kept — nothing re-armed — while the strategy asks for the same
  stop, however many fresh decisions restate it; a replacement never rests
  beside what it replaces. The argument is `oms`'s module docs,
  *Triggers*; don't key anything else on the second half.
- **A venue or harness that cannot rest a trigger refuses the order**,
  never sends it as the plain order it names, which would trade on
  arrival (`fix::ReplaceChain` does).

## The reserved half of the order id space

- **The OMS never mints an id in the reserved half, and passes over a
  report naming one.** `ClientOrderId::RESERVED` — the sequence's top bit —
  is held for another minter in the same process whose executions come back
  on `reports` under its own ids — a request-for-quote responder above the
  order edge, say. That is all this module knows of request for quote: it is
  an order-driven edge, and a quote edge is a separate design. Don't bring a
  quote type, a quote wire or a quote limit in here without that changing.

## The position fold and reconciliation

- **The fold holds no formula for any contract.** It holds a `Measure`
  (linear, linear times a contract multiplier, or inverse) per position,
  from the valuation the caller hands `Book::new`, and the identity is
  `net × measure(mark) − cost`. A new kind of contract is a question for
  the caller's valuation, never another branch in the fold: an index
  future, a bond lot or an FX contract is `Measure::scaled`, not a variant
  of its own. A multiplier that is not positive is not a measure, and the
  fold treats it as none — never value a position at a multiplier of zero.
  `Book` has no `Default`: a book that values nothing must be asked for by
  name.
- **A position is always folded; its value sometimes is not.** A quoter that
  thinks it is flat keeps selling, so the net is never withheld. The money
  is: `pnl` answers `None` where nothing here models the contract, and the
  refusal is counted. Never substitute a zero for either.
- **Currencies are not added together**, and a total that omits an open
  position is not a total. `Book::total` takes one settlement currency and
  refuses where it has no mark for something with a net.
- **Realised PnL is gross of fees.** Fees are their own line because
  attribution splits them out, and a realised number with them
  already folded in could not be split back out.
- **The venue is the truth about positions, and the fold follows it.** A
  difference past `reconcile::Config::grace` re-bases the fold on the venue
  through `Fold::rebase`; the caller latches its risk on every `Mismatch`.
  Never reconcile the other way, never add a tolerance in size (both sides
  are the venue's exact decimals), and never call `Book::rebase` from
  anywhere but a `Fold` the reconciler drives.

## The kill switch

- **A breach latches, and `clear` refuses while one stands.** A limit that
  clears itself when the reading comes back inside is a lag: what usually put
  the book back inside is the market, not a reduction. Clearing is the
  operator's, and clearing over a standing breach would re-latch on the next
  assessment anyway — the refusal is what makes the switch mean what it
  appears to mean. Never add an automatic release; a cooldown is the caller's
  playbook's, and its own halt is released by an operator too.
- **A restart must not clear a latch.** `Switch::snapshot` / `restore`
  carry the latch, what stood and the day's opening equity; a restored
  switch keeps refusing `clear` until an assessment finds the book inside.
- **Which limits, and what each compares, are the caller's.** `kill_switch` holds
  the latch over any `Limit` set and no cap. Read every cap through
  `kill_switch::within` so a value that is not a number is outside it, and record
  an input that could not be evaluated as a breach — a cap nobody evaluated
  has not been respected.

## The venue and the ceiling

- **The ceiling aborts; it never drops or resizes.** `ceiling::Capped`
  wraps a `Venue` and judges every place and amend against the caller's
  `Cap` for its contract; the first one over — or on a contract with no
  cap — fails the run. A dropped request is one the OMS waits on forever,
  and a resized one is a decision the strategy did not make. It wraps both
  venues, so a backtest trips where live would. The judge is stateless: an
  `Amend` carries its contract, because a table of placed ids kept on the
  request edge falls out of step with the OMS's slots (an amend after a
  refused cancel is legitimate) and its one miss would abort the run in the
  middle of a gap.
- **The `Venue` trait is about the edge, not about either venue.** It lives
  with `Request` and `Report`, which is what lets a simulator and a socket
  live where their state does, neither knowing the other. A `Session`
  carries the account beside the reports because risk is *handed* an
  equity and a margin: live they are facts the venue reports, not a model
  anything here runs.
  A venue builds its session as `Session::quiet(reports)` plus a `with_*`
  setter per stream it states, never as a struct literal.
- **A settlement is not a `Report`, on this edge either.** It answers no
  order, and the OMS keys on the orders it sent — so `Session` carries it as
  a `Fill` on its own stream, which the position fold applies and the OMS
  never sees.
- **A cashflow is not a fill.** `Session::cashflows` carries money the
  venue moved on a position with no execution behind it — funding on a
  perpetual, a coupon, a dividend, a borrow fee — positive received; the
  fold books it with `Book::cashflow`, which moves no net, no cost and no
  entry — nothing traded — and keeps it in `Pnl::cashflows` beside the
  fees, so attribution names it rather than the residual absorbing it. It
  never rides the `Report` edge and it never decides anything. A future's
  variation margin is **not** one: the mark already holds it.
- **Every `Session` stream is a forward edge** — the venue's answer, not a
  request. If reconciliation ever seems to need the fold sent *to* a
  venue, that is a feedback edge for no protocol and a design change: the
  only one here is the request wire.

## The FIX shape and the test venue

- **`testing::FixVenue` is a harness, not a model**, and sits behind the
  `execution-testing` feature. It matches against a touch the test states, with
  unlimited depth behind it, so every path the OMS has can be driven exactly. Don't grow it a queue or a book: a test that needs
  one is testing a venue, not the OMS. Its capabilities are `Profile`
  switches, one per way a traditional venue differs (no post-only, no mass
  cancel).
- **A `ClOrdId` is never reused**, and the venue refuses one that is: that is
  the FIX rule a replace chain exists to keep.
- **The OMS never learns a venue id.** `fix::ReplaceChain` owns the map from
  the OMS's `ClientOrderId` to the venue's current `ClOrdId`, and every id
  minted for a live order until a terminal report forgets them all. A venue
  quirk that can be absorbed there goes there, not into `oms`. A request for an order the chain no longer
  holds is answered at once as `UnknownOrder`, never dropped: a slot waiting
  on a message nobody sent waits forever. The other direction is the
  opposite: a venue report naming an id the chain has *forgotten* (a cancel
  refused because the order filled first) is dropped, never passed on as an
  unattributed reject — the OMS reads one of those as its cancel-all refused.
  Only the mass cancel's own `ClOrdId` crosses unattributed.
- **An `Amend` says what shows; a replace's `OrderQty` says the total**, and
  the venue rests it less what has filled. `fix::ReplaceChain` is where the
  two meet: it keeps the venue's `CumQty` per live order (`Trade::filled`,
  carried onto `Fill::filled`) and sends that plus the amend's size — the
  venue's number, never a count of the executions it happened to see. `FixVenue` reads `OrderQty` as FIX does, so a chain that confuses
  them fails `tests/execution_fix_venue.rs`; don't give it the edge's reading back.

## Tests

- Unit tests live beside each module (`#[cfg(test)]`), engine time only.
- `tests/execution_fix_venue.rs`, `#![cfg(feature = "execution-testing")]` —
  the OMS against `testing::FixVenue` through `fix::ReplaceChain`.
- Examples under `examples/adapters/execution/`, each with a README whose
  output is real.
