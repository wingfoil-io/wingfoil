# `market` for execution: identity, arithmetic, money, currency

**Status: §3–§5 built (steps 2–3 of §10); §2 and the lift (steps 4–5) are
not; tracking issue to be filed.** The changes
`adapters::market` needs before the execution layer designed in
[`trading-stack.md`](trading-stack.md) (**Project Venue**) can land as
`adapters/execution/` — one document, because each constrains the others.
Per [`../../README.md`](../../README.md), the tracking issue is the status and
this page carries the reasoning.

Everything below is **additive** to `market`'s public API, and the whole of
it — the lift included — is a **minor** release (§10).

## 1. What the execution layer needs

**Where the code is.** The execution layer is not in this repository. It is
built and tested out of tree, in the private `wingfoil-io/kes` repository
(`crates/kes-exec-core`, read at `17f6f16`), and every file and type this
document names from it — `order.rs`, `fixed.rs`, `position.rs`,
`kill_switch.rs`, `fix.rs`, `exec_id::Inline`, `Switch`, `Measure`, `oms`,
`testing::FixVenue` — is there, not on `main` or in any open PR here. Until
the lift (§10 step 5) brings it in, the claims below about that code are the
author's report of it, not something a reviewer of this repository can
check; the lift PR is where they become checkable.

The execution layer — the order and report vocabulary, an OMS, a position
fold, reconciliation, a kill switch, a FIX-shaped adapter and a test venue,
generic over the instrument and behind off-by-default features (§9) — is
built on `market` and asks four things of it that `market` does not have:

| Need | What it does without them | Where it shows |
|---|---|---|
| A `Copy` instrument identity | `Instrument: Copy + Eq + Hash + Ord + Default + Debug`, a blanket alias, so `Order`, `Fill`, `Request` and `Report` are plain values on a `Burst` | `order.rs`; every type on the edge |
| Exact arithmetic on `Px` and `Qty` | A private `fixed.rs`: `add`, `sub`, `neg`, `abs`, `mul(Qty, Px)`, `div(Qty, Px)`, `scale(a, b, c)` — written to be deleted | `position.rs`, `kill_switch.rs`, `fix.rs`, every consumer |
| A type for an amount of currency | `Qty`, by convention, for fees, PnL, equity, cashflows | `Fill::fee`, `Pnl`, `Position::cost`, `Account::equity` |
| A type for a currency | A closed enum of one venue's codes, the adapter's, reached only through the caller's closures | `Book::total`'s `in_ccy` predicate |

The first is the prerequisite: wingfoil's one identity type, `InstrumentId`,
is two `Arc<str>` and is not `Copy`, so it cannot be the `I` in `Order<I>`,
and PR 964's execution types are built on exactly it. The other three are
what `fixed.rs` and two conventions stand in for, and each is the `Px`/`Qty`
idea — a type per dimension, parsed from the venue's text, never through
`f64` — carried one step further than `market` took it.

Everything below is **additive** to `market`'s public API. No existing type
changes shape, no existing signature changes. That was not obvious at the
start (the obvious move is to make `market`'s events generic over the
identity) and it is the main thing this document settles:
the execution layer can have what it needs without `market`'s events
changing at all.

## 2. Identity: `InstrumentKey`, a `Copy` handle beside `InstrumentId`

### The problem

`InstrumentId { venue: Sym, symbol: Sym }` is right for what it does — it
rides on every market-data message for two atomic increments, and
`same_as` makes the hot compare two pointer checks. It is wrong as a key:

- It cannot be `Copy`, so a value that carries it cannot be, and the
  execution edge's whole design is that its values are.
- It cannot be demuxed on without hashing two strings.
- It cannot say that two venues' names are one contract.

An options desk meets all three with a 32-byte `Copy` contract spec that
carries the contract's economics in the value; `InstrumentId` then appears
only inside the venue adapter and dies at its boundary.

### The decision

**The execution layer stays generic over `I: Copy + Eq + Hash + Ord +
Default + Debug`**, and **`market` gains
`InstrumentKey`**, a `Copy` handle, as the identity a user reaches for when
they have nothing better:

```rust
/// A `Copy` handle for an instrument: what the execution edge keys on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentKey(u32);   // 0 is the Default: no instrument

/// Hands keys out while the graph is wired. The only way to mint one.
pub struct InstrumentsBuilder { ... }
impl InstrumentsBuilder {
    pub fn key(&mut self, id: &InstrumentId) -> InstrumentKey;   // interns
    pub fn freeze(self) -> Arc<Instruments>;
}

/// The frozen registry: resolves keys back, mints none.
pub struct Instruments { ... }
impl Instruments {
    pub fn id(&self, key: InstrumentKey) -> Option<&InstrumentId>;
    pub fn get(&self, id: &InstrumentId) -> Option<InstrumentKey>;
}
```

`Instruments` is the `SymbolInterner` idea applied to the pair, under one
invariant: **one registry per graph.** A key means something only against
the registry that minted it, and two registries would hand out the same
small integers for different instruments with no error on the wrong lookup
— so there is one, built at wiring as subscriptions are made, and frozen
before the run. The types enforce the freeze rather than a comment asking
for it: keys are minted only by `InstrumentsBuilder::key`, which takes
`&mut self`, and `freeze` consumes the builder, so once an op holds the
`Arc<Instruments>` nothing can add to it. Mixing registries is *not*
caught by the type, and this says so rather than implying otherwise: a key
carries no registry id, because one would either double the key's four
bytes or, as a debug-only field, make `Eq` and `Hash` mean different things
in debug and release builds. One registry per graph is a rule the module
docs state, and a key resolved against another registry is a user error
(§11). Keys start at 1; 0 is
`InstrumentKey::default()` and names no instrument, which is what a `Burst`
placeholder holds; `id(InstrumentKey::default())` is `None`, and the
execution layer's maps debug-assert on insert that a key is not the
default, so a placeholder that leaks into a position or an order book
fails loudly in tests. At cycle time an op that needs text — the codec
rendering an order, a log line — holds the `Arc<Instruments>` in its `Cfg`,
read-only, so the resolve is an index into a `Vec` with no lock on the graph
path; an instrument first met after the run has started is the adapter's to
refuse, not to intern (§11 records what that rules out). Keys are dense,
so a `Vec` indexed by key is a map; they are `Ord`, so a `BTreeMap` walk is
a stated order; they are four bytes, so an
`Order<InstrumentKey>` is smaller than an `Order<InstrumentId>` by two
pointers and every clone of it is a copy.

A user with a richer `Copy` identity — a contract spec with the economics
in the value, an equities desk's `(Exchange, Cusip)` — uses it directly and
never meets `InstrumentKey`. A user on `InstrumentId` alone looks a key up
once per subscription. Either way the edge never allocates.

### What this does not do

`market`'s events (`Trade`, `BookUpdate`, `OrderBook`) keep `InstrumentId`.
Making them generic, or switching them to the key, is the larger change
and it is a separate proposal:
it touches every market-data adapter and the Python bindings, and the
execution layer does not need it — the bridge from a book to a `Desired` is
the strategy's, and it holds the key already. Do the small thing now and
find out whether the large one is still wanted after the lift has run.

### Why `Copy`, not `Clone`

The alternative was to relax the bound to `Clone` and let
`InstrumentId` in as it is. The cost is on every user, not just that one:
`Slot`, `Plan`, `Desired`, `Placed` and every `.copied()` walk in the OMS
become clones, and "the edge never allocates" stops being a property of the
crate and becomes a property of the caller's `I`. `InstrumentKey` makes
`Copy` free for the user who had nothing, and everyone else already had it.

## 3. Arithmetic: the operators `fixed.rs` exists to be replaced by

`Px` and `Qty` are `i128` newtypes at a shared `10^9` scale with no `Add`,
`Sub`, `Mul` or `Div`. Every consumer that computes leaves through `raw()`
or `to_f64()`, and neither carries a scale. The execution layer's
`fixed.rs` is the list of operations a consumer actually needs, written to
be deleted when these land.

### The set

Not blanket impls — `Px + Px` is dimensionally meaningless and should not
compile. The operations that mean something, each checked:

| Operation | Result | Why it exists |
|---|---|---|
| `Qty ± Qty`, `-Qty`, `Qty::abs` | `Qty` | a position moves by a fill |
| `Px - Px` | `Px` | a spread, a move |
| `Px::midpoint(a, b)` | `Px` | replaces `OrderBook::mid`'s `f64`; exact, rounds toward zero |
| `Amount::of(Qty, Px)` | `Option<Amount>` (§4) | `qty × price`: a notional, a cost, a fee base |
| `Amount::inverse(Qty, Px)` | `Option<Amount>` | `qty / price`: what a contract quoted in one currency and settled in the other is worth in the one it settles in. A named function, not `Div`, because it is a contract convention, not division |
| `Amount ± Amount`, `-Amount`, `Amount::abs` | `Amount` | PnL lines |
| `Amount::convert(Px)` | `Option<Amount>` | the same amount in another currency at a rate, applied by multiplying: the rate is units of the target currency per unit of this one. A rate is a price — of one unit of a currency, in another — so `Px` is its dimension; which two currencies is `Money`'s to check (§5), not the type's |
| `Amount::per(Qty)` | `Option<Px>` | `amount / qty`: an average entry price |
| `Qty::scaled(Scalar)` | `Option<Qty>` | a contract multiplier — units of price per contract, an index future at fifty a point. A multiplier is dimensionless, so it is a `Scalar`, a fourth type from the same macro, and not a `Qty`: `Qty × Qty` would be units², and the argument at the top of this section applies to it too |
| `mul_div(a, b, c)` on the raw integers | `Option<i128>` | `a × b / c`, multiply first, for the rate applied without leaving fixed point |

### The rules, which are `fixed.rs`'s

- **Checked, never wrapping, never saturating.** A product or a quotient,
  where overflow is reachable at `10^18` scale, is a function answering
  `Option` — `None` on overflow or a zero divisor — because a number that
  cannot be computed is one the caller has to have a plan for, and a
  saturated value is a plausible wrong one. A sum or a difference, where
  overflow is not reachable from two representable values in any book, is an
  operator, and it is `checked_*().expect("invariant: …")`: a loud panic on
  the impossible rather than a silent wrap. That matters because the root
  `[profile.release]` leaves `overflow-checks` off, so a plain `a + b` on
  the raw integers wraps in release; `OrderBook::spread` does exactly that
  today and moves onto the operator.
- **Multiply before dividing.** `qty × SCALE / price` keeps nine significant
  digits where the reciprocal first keeps four; a position fold that did it
  the other way reported a $60,000 entry as $60,002.40. `mul_div` is the one
  primitive and everything else is a call to it.
- **Nothing goes through `f64`.** `to_f64` stays as the bridge to the
  `statistics` ops and stops being the only way to multiply.
- **Rounding is toward zero**, which is what `i128` division does and is
  said on every function that rounds. It is a choice, not a precedent:
  `parse` does not round at all — it refuses a digit past `DECIMALS` — and
  `try_from_f64` rounds to nearest, for an `f64` that already carries
  error. Toward zero is chosen because it never manufactures magnitude: a
  rounded result is never larger than the exact one. That is not the same
  as conservative — a loss or a fee paid rounds smaller too, in the
  holder's favour — and it is not a side of the book: a mid of two positive
  prices rounds down, toward the bid, but a mid of two negative prices
  rounds up, toward the ask. `Px::midpoint` is `i128::midpoint`, which
  rounds toward zero on every pair of signs and cannot overflow.

`OrderBook` gains `mid_px() -> Option<Px>`, exact, beside `mid()`, which
is marked `#[deprecated]` pointing at it and otherwise left alone — a
signature change on a published method is a major bump, and a deprecation
is not. The deprecation is not free: `cargo lint` is `-D warnings` across
every target, so every in-tree caller — the module's own doctests and unit
tests, `tests/market_adapter.rs` ×3, the market example — moves to `mid_px`
in the same PR, the example keeping its `f64` display through
`.map(Px::to_f64)` so its pinned README output does not change.
(`benches/pooled_channel.rs`'s three `mid()` calls are on the bench's own
`Book` struct, not `OrderBook`'s, and are untouched.) `microprice` is a
weighted average and not a `Px`; it keeps `f64` and says so. Both removals
ride the next major, whenever one is cut for its own reasons.

### `NanoTime` is not in this proposal

`NanoTime + Duration` is a deliberately unchecked `u64` add
(`runtime/time.rs`), and `Duration::MAX` — the obvious spelling of "off" for
a threshold — overflows it. The execution layer spells every threshold as
an elapsed `Duration` compared against the limit, never as `as_of + limit`,
and that idiom is stated in the execution module's docs. A `checked_add` on
`NanoTime` would be welcome and is not required.

## 4. Money: `Amount`

### The type

A third fixed-point type from `market`'s `fixed_point!` macro, the same
`i128` at the same `10^9` scale:

```rust
fixed_point!(Amount, "amount of currency");
```

An `Amount` is a signed quantity of *some* currency: a fee, a realised or
unrealised PnL, a cost basis, an equity, a cashflow. It is the third
dimension beside `Px` (a price of one unit) and `Qty` (a count of units),
and with it the arithmetic in §3 is typed by dimension: `Qty × Px` is the
way to make one, `Qty + Amount` does not compile, and the position fold's
identity `net × measure(mark) − cost` is typed end to end. What the type
does not check is the currency — `Amount::convert` changes it and two
`Amount`s in different currencies add — which is §5's job.

### The name

PR 964 calls this type `Notional`. That is the wrong word: in trading a
notional is a position's face value, price times size, gross and unsigned —
the thing a fee schedule is a fraction of and an exposure limit caps. 964
uses it for a fee, a realised PnL, a cost basis and an equity, none of which
is a notional; the name was drawn from the one call site that mints it and
stretched over everything denominated in currency. **The type is `Amount`**,
and `notional` stays free for the derived quantity that means it:
`Amount::notional_of(price, qty)` or a `Position::notional()`.

### What becomes an `Amount` in the lifted module

`Fill::fee`, `Cashflow::amount`, `Account::equity`; `Pnl`'s four lines;
`Position::cost`, `realised`, `fees`, `cashflows`, `booked`;
`Measure::value`'s answer; `Book::total`; `Switch::daily_loss` and
`Day::opening`. `Measure::Scaled { multiplier }` becomes a `Scalar` (§3):
units of price per contract is neither money nor a size.

### What it does not carry

A currency. The rule stays that an `Amount` beside a position is in that
position's instrument's settlement currency, and a field naming it could
only ever disagree with the instrument. Where an amount stands alone, §5.

## 5. Currency: `Ccy`, and `Money` where an amount stands alone

### The type

An open, `Copy` currency code, inline:

```rust
/// A currency code as the venue spells it — `USD`, `EUR`, `JPY`, `USDC` —
/// held inline. Empty is the `Default` and names no currency.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ccy { bytes: [u8; 7], len: u8 }   // 8 bytes

impl Ccy {
    pub fn parse(s: &str) -> Result<Ccy>;   // refuses empty, > 7 bytes, non-ASCII
    pub fn as_str(&self) -> &str;
}
```

The same construction as the execution layer's `exec_id::Inline`, much
shorter. Open, because a closed enum is a venue's list — right for that
venue, useless for the next, and the adapter's to own, which the execution
layer may never name. Seven bytes holds every ISO 4217 code (three letters)
and the longer codes some venues spell, and a venue that spells one longer
still is refused at the decode boundary rather than truncated, as an
`ExecId` is.

Two facts about a currency that are not the type's: which currency an
instrument settles in (`settles_in(&I) -> Ccy`) is the caller's, handed in
as `Measure` is; and the quote currency of a contract that settles in its
base is a unit that is never settled, so a `Ccy` on an `Amount` is a
*settlement* currency and the docs say so.

### `Money`: an amount with its currency

```rust
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Money { pub amount: Amount, pub ccy: Ccy }
```

For the places an amount stands alone and nothing beside it says the
currency: `Account::equity` (a multi-currency account is a `Burst<Money>`),
`Book::total(ccy, mark) -> Option<Money>` (replacing the `in_ccy`
predicate), a `Cashflow` on an instrument the book has never held, the kill
switch's daily loss. `Money + Money` is a `Result`, refused on a mismatch
— and refused when either side's `Ccy` is empty: `Money::default()` names no
currency, and adding to it would let an unlabelled amount pick up a label
from whatever it meets. The one legitimate change of currency is

```rust
impl Money {
    /// `self` in `to`, at `rate` units of `to` per unit of `self.ccy`.
    pub fn convert(self, rate: Px, to: Ccy) -> Option<Money>;
}
```

which multiplies, through `Amount::convert`. The target currency is an
argument because a `Px` carries none: the rate's dimension is a price, and
which pair it prices is the caller's to state. `None` if the product does not
fit or either code is empty. Inside a `Position`, `Amount` stays bare: the instrument is
right there and a tag would be a second copy of one fact.

Three options were weighed. A compile-time `Amount<C>` with a phantom
currency is the `Px`/`Qty` argument taken one step further and it fails
here because a currency is data off the wire, not a type a generic
`Order<I>` can know. A tag on every `Amount` costs eight bytes on every
field of every `Pnl` for a fact the instrument already states. `Money` at
the boundary and `Amount` inside is what accounting libraries do, and it is
where `Book::total`'s `in_ccy` predicate already draws the line.

## 6. The shared scale, left alone

`Px`, `Qty` and now `Amount` share one `DECIMALS = 9` on an `i128`. The doc
on `fixed_point!` says why: price wants precision at modest magnitude and
quantity wants magnitude at modest precision, and the type resolves it by
widening rather than by giving the two different scales. The costs are real
— every book key pays sixteen bytes and a two-word compare for magnitude
only `Qty` needs, and one global `DECIMALS` means two venues at different
precisions cannot coexist in one process — and they are documented. This
proposal does not change it. `Amount` at `10^9` holds `1.7 × 10^20` of any
currency, which is enough, and the position fold's two-step scaled multiply
(`qty × multiplier` first, then `× price`) is how a product of three scaled
numbers stays inside it.

## 7. The `thiserror` exception

wingfoil's rule is `anyhow::Result` everywhere, `.context()` at I/O
boundaries. The execution layer's rule is that every error is a type a
caller can match on — `OrderError`, `RequestError`, `IdError`, `Breach<I>`,
`ClearError<L>`, `rate_limit::Invalid` — with the message stating the fact
and the argument on the variant. That is the better design, and PR 964's
`anyhow` strings the worse: an OMS refusing an order is not an I/O
boundary, it is a value a graph routes on.

So `execution` enables `dep:thiserror`. It is already an optional dependency
(`iceoryx2` enables it today), `log` and `anyhow` are unconditional, and
nothing else is needed. The exception is stated once, in the module's
`CLAUDE.md`, with this paragraph as the reason.

## 8. PR 964

`wingfoil-io/wingfoil#964` is gate P0 of Project Venue: `Order`, `Fill`,
`Notional`, a per-instrument `Position` op and a fill-at-touch `SimVenue`,
open since 13 September with two commits. `trading-roadmap.md` still defers
the OMS "until a real strategy demands it"; this proposal is that demand.

**The proposed decision: 964 is rebased onto the execution layer's
vocabulary and becomes the `execution-sim` half of one landing.** It is
recorded on #964 itself, and this document does not merge until #964's
author has agreed there (§10 step 1). Its `#[op]` node form, its
graph-level loop test with pinned tick times, its module `CLAUDE.md` format
and its example-with-real-output survive, and are the shape the lifted
module copies; its money type survives as `Amount` (§4). Its `Order` and
`Fill` (not `Copy`, string ids that allocate per order, `Day` as the default
lifetime, no cancel or amend), its per-instrument `Position` op that adopts
the first fill's instrument silently, and its `validate` returning `anyhow`
strings do not: the execution layer's (out of tree, §1) are the tested, venue-neutral
versions of the same types, with the fields 964 rightly insisted on (`cum_qty` as
`Fill::filled`, `leaves_qty` as `remaining`, both timestamps).

This closes Project Venue §12's open questions: order state lives in an OMS
op with one resting order per instrument and side, keyed on the instrument,
and the order edge carries `Burst<Request>`.

## 9. The feature split

Project Venue §7.2's names, with the execution layer's modules placed:

| Feature | Modules | Enables |
|---|---|---|
| `execution` | `order`, `edge`, `exec_id`, `position`, `venue`, `ceiling`, `kill_switch`, `rate_limit` | `market`, `dep:thiserror` |
| `execution-oms` | `oms` (with `oms::node`), `reconcile`, `fix` | `execution` |
| `execution-sim` | 964's `SimVenue`, rebased onto `Request`/`Report` and the `Venue` trait | `execution` |
| `execution-testing` | `testing::FixVenue` | `execution-oms` |

All off by default and out of the prelude, per §7.3's rule that the
default-feature public API grows no trading type. `fixed.rs` does not
travel: its call sites become §3's operators. The OMS design notes go to
`docs/decisions/` **in the lift PR itself** (§10 step 5), since
`decisions/` is for what is true of `main` and they are not until the code
they describe is. The first screen of the
module docs states the boundary: an order-driven edge and a quote manager
with one resting order per instrument and side; the request-for-quote edge,
order reconciliation on connect and depth quoting are three designs it does
not include.

## 10. Order of work, and the version

Each is one PR on wingfoil. **Everything is additive** — new types, new
trait impls on existing types, new inherent methods, a new off-by-default
feature, one deprecation — so the whole of it, the lift included, ships in
a **minor** release (9.1.0), not a major. The `bump.yml` dispatch is
`minor`. What would have made it a major, and is kept out on purpose:
changing `mid()`'s return type (§3), removing `microprice`, and touching
`market`'s event types (§2, deferred). MSRV stays 1.88: nothing in the
execution layer needs a floor wingfoil does not have.

1. **This document**, and the decision on 964 recorded on its PR.
2. **§3 and §4 together**: the operators, `Amount` and `Scalar`. One PR
   because the cross-dimension operators are `Amount`'s constructors.
   `mid_px` beside a deprecated `mid`, the eight callers moved (two
   doctests, two unit tests, three in `tests/market_adapter.rs`, the
   example), nothing
   removed. Open as #986.
3. **§5**: `Ccy` and `Money`. Landed; `Money` sits in `market`, beside
   `Ccy` (§11's open question, settled that way for now).
4. **§2**: `InstrumentKey` and `Instruments`.
5. **The lift**: `adapters/execution/`, generic over `I`, with 964 rebased
   into `execution-sim`, per §9, and the OMS design notes into
   `docs/decisions/` in the same PR.

Steps 2 to 4 can land in any order and before the lift is scheduled; each
is useful to `market` on its own.

## 11. Open

- **Instruments that appear mid-run.** The registry is frozen before the
  run (§2), so an instrument first seen after it starts is refused. That
  rules out real cases — an options venue listing new strikes or expiries
  intraday, a new future rolled in — which today need a restart to trade.
  The likely answer is an epoch: a new frozen `Instruments` swapped in at a
  cycle boundary, keys stable across it. Not designed until a venue in the
  lift needs it.
- **Keys from two registries.** Not caught (§2). If it bites in the lift, a
  registry id in the high bits of the `u32` is the cheapest check — it
  costs key space, not size.
- Whether `market`'s events take `InstrumentKey` later. Decide after the
  lift has run a venue on the key.
- Per-type `DECIMALS` (§6). Not until a venue needs it.
- `Ccy` at seven bytes. Wide enough for every code met so far; the parse
  refuses rather than truncates, so a wider one is a constant, not a wire
  change.
- Whether `Money` is in `market` or in `execution`. `Ccy` is `market`'s; the
  pair is used only by the execution layer today, and could start there.
