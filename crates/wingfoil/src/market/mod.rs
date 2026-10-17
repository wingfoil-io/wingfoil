//! Market — the venue-neutral trading layer, in two halves:
//!
//! - [`data`] — the market data vocabulary ([`Px`], [`Qty`], [`Trade`],
//!   [`BookSnapshot`], …) and order book maintenance
//!   ([`MarketBookOps::order_book`]), behind the `market` feature.
//! - [`execution`] — the execution layer on those types: the order edge, an
//!   OMS, the position fold, reconciliation, the kill switch and the
//!   [`Venue`](execution::venue::Venue) swap point, behind the `execution`
//!   feature (which implies `market`).
//!
//! Every item of [`data`] is re-exported here, so the 9.0 paths
//! (`wingfoil::market::Px`, `…::market::MarketBookOps`) keep
//! working unchanged; prefer them for the data vocabulary.

pub mod data;
#[cfg(feature = "execution")]
pub mod execution;

pub use data::*;
