//! Trading — the venue-neutral trading layer, in two halves:
//!
//! - [`market`] — the market data vocabulary ([`Px`](market::Px),
//!   [`Qty`](market::Qty), [`Trade`](market::Trade), …) and order book
//!   maintenance ([`MarketBookOps`](market::MarketBookOps)), behind the
//!   `market` feature.
//! - [`execution`] — the execution layer on those types: the order edge, an
//!   OMS, the position fold, reconciliation, the kill switch and the
//!   [`Venue`](execution::venue::Venue) swap point, behind the `execution`
//!   feature (which implies `market`).
//!
//! Neither connects to anything: venue integrations normalise into
//! [`market`] and implement [`Venue`](execution::venue::Venue), so a graph
//! wired against one venue runs unchanged against another.
//!
//! [`market`] is also reachable at its 9.0 path,
//! [`adapters::market`](crate::adapters::market), until the next major
//! release.

#[cfg(feature = "execution")]
pub mod execution;
pub mod market;
