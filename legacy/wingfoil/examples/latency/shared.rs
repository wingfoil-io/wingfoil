// Shared payload + latency schema used by the latency_pub and latency_sub
// example binaries. Both processes must agree on the layout — the
// `latency_stages!` macro guarantees this when both sides declare the same
// stages in the same order.
use iceoryx2::prelude::ZeroCopySend;
use wingfoil::*;

/// The service the two processes connect on.
pub const SERVICE_NAME: &str = "wingfoil/examples/latency";

/// The business payload — must be `#[repr(C)]` and `ZeroCopySend` for iceoryx2.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, ZeroCopySend)]
pub struct Quote {
    pub seq: u64,
    pub price: f64,
}

// Latency schema, shared across processes. Stage order matters: it
// determines the offsets in the on-wire `[u64; N]` view of the record.
latency_stages! {
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
