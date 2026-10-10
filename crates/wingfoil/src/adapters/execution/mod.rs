//! Execution — the venue- and asset-neutral layer between a strategy and a
//! venue: the order edge, an OMS, a position fold, a reconciler, a kill switch
//! and the swap point a venue implements.
//!
//! Nothing here knows an asset class or a venue: the instrument is the
//! caller's type (any `Copy + Eq + Hash + Ord + Default + Debug`), each
//! contract's economics are handed in, and a venue is an implementation of
//! [`venue::Venue`]. What is specific to one asset class or one venue lives in
//! the crate that implements it. Like [`market`](crate::adapters::market) it
//! connects to nothing, and it is behind the off-by-default `execution`
//! feature and out of the [`prelude`](crate::prelude):
//! `use wingfoil::adapters::execution::oms::OmsOps;`.
//!
//! # What it is, and what it is not
//!
//! An **order-driven** edge — [`Request`](edge::Request) out,
//! [`Report`](edge::Report) back — and an OMS that keeps a ladder of resting
//! orders per instrument and side, with parent orders ([`algo`]) as a layer
//! above it. It does not include a request-for-quote or streaming-quote edge
//! (most OTC rates, credit and FX venues), reconciliation of the venue's
//! working orders on connect, or a venue simulator: each is a design of its
//! own, built beside this one rather than into it.
//!
//! - [`algo`] — parent orders over the OMS: `Parent` worked as TWAP clips,
//!   an iceberg or one capped sweep, emitting the OMS's desireds and reading
//!   its fills back.
//! - [`order`] — `Order`, `Fill` and their kinds, generic over the
//!   instrument, and `ClientOrderId` with its process `Epoch`.
//! - [`edge`] — every value that crosses the swap point: `Request` out;
//!   `Report`, `Account`, `Holdings`, `Cashflow`, `MmpTrip` and
//!   `TradingState` back. Generic the same way.
//! - [`position`] — `Book<I>` and `Position<I>`, the fold over fills, with
//!   each instrument's `Measure` (linear, scaled by a contract multiplier, or
//!   inverse) handed in by the caller.
//! - [`reconcile`] — `Reconciler<I>`: a position [`Fold`](reconcile::Fold)
//!   read against the venue's holdings, and re-based on it past a grace.
//! - [`kill_switch`] — `Switch<L>`, the kill switch's latch over any set of
//!   limits, and the day a loss is measured over.
//! - [`venue`] — `Venue<I>`, the swap point, and the `Session<I>` a venue
//!   answers with: one stream per audience of what [`edge`] defines.
//! - [`ceiling`] — `Capped`, a venue whose request edge aborts the run on an
//!   order over the caller's per-contract `Cap`.
//! - [`fix`] — the shape FIX imposes on order entry: a new `ClOrdId` per
//!   message, execution reports that name the message they answer.
//! - `testing` — `FixVenue`, a FIX-shaped test venue without post-only or
//!   mass cancel: the harness for the OMS against a traditional venue.
//!   Behind the `execution-testing` feature, so it is not part of the API a
//!   user builds against.
//! - [`exec_id`] — the venue's inline ids: `ExecId` for an execution and
//!   `VenueId` for an order, bounded, `Copy`.
//! - [`rate_limit`] — `OrderRate`, a venue's order-entry limits as stated and the
//!   headroom spent under them, and `Bucket`, the engine-time token bucket
//!   that meters them.

pub mod algo;
pub mod ceiling;
pub mod edge;
pub mod exec_id;
pub mod fix;
pub mod kill_switch;
pub mod oms;
pub mod order;
pub mod position;
pub mod rate_limit;
pub mod reconcile;
pub mod venue;

#[cfg(feature = "execution-testing")]
pub mod testing;
