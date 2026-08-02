//! Shared payload + latency schema used by the `latency_pub` and `latency_sub`
//! example binaries.
//!
//! A port of legacy `legacy/wingfoil/examples/latency/shared.rs` onto the next
//! engine. The data layer is unchanged — `Traced` and `latency_stages!` are
//! engine-agnostic and re-exported from the legacy crate by
//! `wingfoil_next::latency` — so this file is byte-for-byte the legacy one
//! but for the import path.
//!
//! Both processes must agree on the layout: the `latency_stages!` macro
//! guarantees this when both sides declare the same stages in the same order.
//!
//! # Deviation from legacy: the `#[type_name(...)]` overrides are required
//!
//! Both payload types below pin an explicit iceoryx2 `type_name`, which the
//! legacy example omits. Without them the default `ZeroCopySend::type_name()`
//! is `core::any::type_name::<Self>()`, which embeds the *absolute Rust path*
//! of the type — and because this file is `#[path]`-included by two separate
//! binary crates, that path is `latency_pub::shared::Quote` in one process and
//! `latency_sub::shared::Quote` in the other. iceoryx2 compares those strings
//! when opening the service and rejects the second process with
//! `IncompatibleTypes`, even though the memory layouts are identical.
//!
//! Legacy documents this exact hazard on its `Traced<T, L>` `ZeroCopySend`
//! impl (`legacy/wingfoil/src/latency.rs`) and provides `#[type_name(...)]` to escape
//! it, but its own `examples/latency` never applies it — so the legacy pair
//! aborts at publisher start. Pinning the names here is a deliberate fix, not
//! a divergence in what the example demonstrates.

use iceoryx2::prelude::ZeroCopySend;
// `latency_stages!` expands to impls of `Latency` / `Stage`, so both traits
// must be in scope at the expansion site (legacy has the same requirement —
// it gets them from its `use wingfoil_next::*` prelude glob).
use wingfoil_next::latency::{Latency, Stage, latency_stages};

/// The service the two processes connect on.
pub const SERVICE_NAME: &str = "legacy/wingfoil/examples/latency";

/// The business payload — must be `#[repr(C)]` and `ZeroCopySend` for iceoryx2.
///
/// The `type_name` is pinned so both binaries agree on it — see the
/// module docs.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
#[type_name("wingfoil::examples::latency::Quote")]
pub struct Quote {
    pub seq: u64,
    pub price: f64,
}

// Latency schema, shared across processes. Stage order matters: it
// determines the offsets in the on-wire `[u64; N]` view of the record.
latency_stages! {
    #[type_name("wingfoil::examples::latency::QuoteLatency")]
    pub QuoteLatency {
        // publisher side
        produce,
        publish,
        // subscriber side
        receive,
        strategy,
        ack,
    }
}
