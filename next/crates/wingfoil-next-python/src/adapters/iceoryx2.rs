//! Python bindings for the wingfoil-next **iceoryx2** adapter
//! ([`wingfoil_next::adapters::iceoryx2`]).
//!
//! Two graph entry points, both `#[pyadapter]`-generated:
//!
//! | Python                     | Rust                                          | shape |
//! |----------------------------|-----------------------------------------------|-------|
//! | `iceoryx2_sub(graph, …)`   | [`iceoryx2_sub_slice_opts`]                   | service → `list[bytes]` |
//! | `iceoryx2_pub(stream, …)`  | [`Iceoryx2SliceSinkOps::iceoryx2_pub_slice_opts`] | service sink |
//!
//! # The slice API, not the typed one
//!
//! The engine has two surfaces: a *typed* one over `ZeroCopySend` records, and a
//! *slice* one over `[u8]`. Python has no `ZeroCopySend` type to name — it is a
//! compile-time trait on a `#[repr(C)]` layout — so the binding uses the slice
//! API, exactly as the legacy one did. Payloads cross as raw **`bytes`**; each
//! sample is copied out of the shared-memory loan immediately, so a
//! variable-length payload needs no type of its own.
//!
//! # Not in the default wheel
//!
//! iceoryx2 is pure Rust and needs no extra build toolchain, so it *is* in the
//! `all-adapters` roll-up and its tests run in the normal job. It is kept out of
//! the **wheel**'s default features for a different reason: it is
//! Linux/POSIX-only, and a wheel that carries it cannot be built for the
//! platforms that would otherwise work. `maturin develop -F iceoryx2` opts in.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **`variant` and `mode` are strings**, not `Iceoryx2ServiceVariant` /
//!    `Iceoryx2Mode` `#[pyclass]` enums — the convention postgres set. Two fewer
//!    classes to register, and an unknown value raises listing what is accepted.
//! 2. **The run mode is an argument.** An iceoryx2 subscription is a live,
//!    unbounded source, so a historical run is rejected at wiring; a Python
//!    `Graph` does not know its mode until `run()`, so `realtime` is explicit.
//! 3. **Publishing rejects a non-bytes value loudly.** The burst seam's edge
//!    conversion is what enforces it, so the error names the type it got and
//!    aborts the run — legacy hand-rolled the same check in the binding.
//! 4. **The `stages` latency-tracing path is not ported.** Legacy's `stages`
//!    argument split a `[u64; N]` header off each sample into a `TracedBytes` /
//!    `Latency` pyclass pair. Those types belong to legacy's `latency` module,
//!    which has no next equivalent yet; the tracing port is where they will
//!    return, not here. Everything else legacy exposed is covered.

use anyhow::{Result, bail};
use wingfoil_next::adapters::iceoryx2::{
    ICEORYX2_DEFAULT_HISTORY_SIZE, ICEORYX2_DEFAULT_INITIAL_MAX_SLICE_LEN, Iceoryx2Mode,
    Iceoryx2PubSliceOpts, Iceoryx2ServiceVariant, Iceoryx2SliceSinkOps, Iceoryx2SubOpts,
    iceoryx2_sub_slice_opts,
};
use wingfoil_next::prelude::{Burst, GraphBuilder, Stream};

use crate::adapters::common::run_mode;
use crate::pyadapter;

/// The service-variant selector, a string rather than a `#[pyclass]` enum.
fn variant(name: &str) -> Result<Iceoryx2ServiceVariant> {
    match name {
        "ipc" => Ok(Iceoryx2ServiceVariant::Ipc),
        "local" => Ok(Iceoryx2ServiceVariant::Local),
        other => bail!("iceoryx2: unknown variant '{other}'; expected 'ipc' or 'local'"),
    }
}

/// The polling-mode selector, a string rather than a `#[pyclass]` enum.
fn poll_mode(name: &str) -> Result<Iceoryx2Mode> {
    match name {
        "spin" => Ok(Iceoryx2Mode::Spin),
        "threaded" => Ok(Iceoryx2Mode::Threaded),
        "signaled" => Ok(Iceoryx2Mode::Signaled),
        other => {
            bail!("iceoryx2: unknown mode '{other}'; expected 'spin', 'threaded' or 'signaled'")
        }
    }
}

/// Subscribe to an iceoryx2 byte-slice service.
///
/// Each tick yields a `list` of `bytes` — the samples received between graph
/// cycles, losslessly grouped.
///
/// `variant` is `"ipc"` (shared memory, across processes) or `"local"`
/// (in-process, over the heap). `mode` is `"spin"` (poll inside the graph
/// cycle — lowest latency, one core), `"threaded"` (a background thread, one
/// channel hop) or `"signaled"` (a blocking `WaitSet`, which needs the
/// publisher to signal on the matching Event service). `history_size` is how
/// many past samples a late joiner receives, and is part of the **service
/// contract**: every participant on the same service must agree.
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`; a subscription is live and unbounded with
/// no historical timeline, so `realtime=False` raises here.
///
/// The subscriber port is created at graph `start()`, so an invalid service
/// name or a contract mismatch aborts the run rather than raising at wiring.
#[pyadapter(name = iceoryx2_sub, source)]
#[pyo3(signature = (
    service_name, realtime = true, variant = "ipc".to_string(),
    mode = "spin".to_string(), history_size = ICEORYX2_DEFAULT_HISTORY_SIZE,
))]
fn sub(
    g: &GraphBuilder,
    service_name: String,
    realtime: bool,
    variant: String,
    mode: String,
    history_size: usize,
) -> Result<Stream<Burst<Vec<u8>>>> {
    let opts = Iceoryx2SubOpts {
        variant: self::variant(&variant)?,
        mode: poll_mode(&mode)?,
        history_size,
    };
    iceoryx2_sub_slice_opts(g, run_mode(realtime), &service_name, opts)
}

/// Publish this stream to an iceoryx2 byte-slice service.
///
/// Each tick's value is `bytes` for one sample, or a `list`/`tuple` of `bytes`
/// to send several on that tick. A value that is not bytes-like aborts the run.
///
/// `initial_max_slice_len` bounds the largest sample the publisher can loan;
/// it and `history_size` are part of the service contract (see
/// [`iceoryx2_sub`](sub)). The publisher port is created at graph `start()`.
///
/// Returns a terminal stream whose value is `None`.
#[pyadapter(name = iceoryx2_pub)]
#[pyo3(signature = (
    service_name, variant = "ipc".to_string(),
    history_size = ICEORYX2_DEFAULT_HISTORY_SIZE,
    initial_max_slice_len = ICEORYX2_DEFAULT_INITIAL_MAX_SLICE_LEN,
))]
fn publish(
    stream: &Stream<Burst<Vec<u8>>>,
    service_name: String,
    variant: String,
    history_size: usize,
    initial_max_slice_len: usize,
) -> Result<Stream<()>> {
    if initial_max_slice_len == 0 {
        bail!("iceoryx2_pub: initial_max_slice_len must be at least 1");
    }
    let opts = Iceoryx2PubSliceOpts {
        variant: self::variant(&variant)?,
        history_size,
        initial_max_slice_len,
    };
    Ok(stream.iceoryx2_pub_slice_opts(&service_name, opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variant_selector_is_a_string() {
        assert_eq!(Iceoryx2ServiceVariant::Ipc, variant("ipc").unwrap());
        assert_eq!(Iceoryx2ServiceVariant::Local, variant("local").unwrap());
        let err = variant("shm").unwrap_err();
        assert!(
            err.to_string().contains("expected 'ipc' or 'local'"),
            "{err}"
        );
    }

    #[test]
    fn the_mode_selector_is_a_string() {
        assert_eq!(Iceoryx2Mode::Spin, poll_mode("spin").unwrap());
        assert_eq!(Iceoryx2Mode::Threaded, poll_mode("threaded").unwrap());
        assert_eq!(Iceoryx2Mode::Signaled, poll_mode("signaled").unwrap());
        let err = poll_mode("polling").unwrap_err();
        assert!(
            err.to_string()
                .contains("expected 'spin', 'threaded' or 'signaled'"),
            "{err}"
        );
    }

    #[test]
    fn the_defaults_match_the_engines() {
        // The signature's defaults are the engine constants, not copies — a
        // contract mismatch between a Python and a Rust participant on the same
        // service is exactly the failure this guards.
        assert_eq!(5, ICEORYX2_DEFAULT_HISTORY_SIZE);
        assert_eq!(128 * 1024, ICEORYX2_DEFAULT_INITIAL_MAX_SLICE_LEN);
        let opts = Iceoryx2SubOpts::default();
        assert_eq!(ICEORYX2_DEFAULT_HISTORY_SIZE, opts.history_size);
        assert_eq!(Iceoryx2ServiceVariant::Ipc, opts.variant);
        assert_eq!(Iceoryx2Mode::Spin, opts.mode);
    }
}
