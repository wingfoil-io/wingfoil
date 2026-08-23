//! Python bindings for the wingfoil **Aeron** adapter
//! ([`wingfoil::adapters::aeron`]).
//!
//! Four graph entry points, all `#[pyadapter]`-generated:
//!
//! | Python                             | Rust                             | shape |
//! |------------------------------------|----------------------------------|-------|
//! | `aeron_sub(graph, …)`              | [`aeron_sub_fragment`]           | subscription → `list[bytes]` |
//! | `aeron_sub_with_status(graph, …)`  | [`aeron_sub_fragment_with_status`] | the same, plus a status stream |
//! | `aeron_pub(stream, …)`             | [`AeronSinkOps::aeron_pub`]      | publication sink |
//! | `aeron_pub_with_status(stream, …)` | [`AeronSinkOps::aeron_pub_with_status`] | the same, plus a status stream |
//!
//! # Not in the default wheel
//!
//! `rusteron-client` builds the Aeron **C** library from source, needing clang,
//! libuuid and CMake ≥ 3.30. So — alone with iceoryx2 — this binding is *not*
//! in the `all-adapters` roll-up and *not* in the wheel's default feature set:
//! shipping it would make the published wheel un-buildable for everyone else.
//! Build it explicitly with `maturin develop -F aeron`; its tests run in
//! `aeron-integration.yml`, which installs that toolchain.
//!
//! # Connecting is eager
//!
//! Unlike the string-addressed adapters, Aeron needs a running **media driver**
//! and resolves `(channel, stream_id)` through it. The handle is obtained at
//! *wiring*, so an unreachable driver or an unresolvable subscription raises
//! there rather than aborting a later run — the same eager shape the legacy
//! binding had.
//!
//! # The payload
//!
//! Fragments cross as raw **`bytes`**, as legacy and zmq do. A subscription is
//! burst-shaped, so each tick yields a `list` of the fragments that arrived in
//! that cycle; a publication accepts a `list`/`tuple` of `bytes` to send several
//! messages on one tick, or a single `bytes` for one.
//!
//! # Deviations from the legacy `wingfoil-python` bindings
//!
//! 1. **`mode` is a string**, `"spin"` or `"threaded"`, not an `AeronMode`
//!    `#[pyclass]` enum — the convention postgres set. One fewer class to
//!    register, and an unknown value raises listing what is accepted.
//! 2. **The run mode is an argument.** A subscription is a live, unbounded
//!    source, so a historical run is rejected at wiring; a Python `Graph` does
//!    not know its mode until `run()`, so `realtime` is explicit.
//! 3. **Publishing fails loudly.** Legacy's `extract::<Vec<u8>>(py)
//!    .unwrap_or_else(|e| { log::error!(…); Vec::new() })` published an *empty
//!    message* when the value was not bytes. Here the run aborts.
//! 4. **New capabilities**: the `_with_status` pair (legacy bound neither),
//!    `fragment_limit`, and multi-message publish from one tick — legacy wrapped
//!    each value in a one-element burst.

use std::time::Duration;

use anyhow::{Result, bail};
use wingfoil::adapters::aeron::{
    AeronHandle, AeronMode, AeronSinkOps, AeronStatus, AeronSubOptions, DEFAULT_FRAGMENT_LIMIT,
    FragmentBuffer, TransportError, aeron_sub_fragment, aeron_sub_fragment_with_status,
};
use wingfoil::prelude::{Burst, GraphBuilder, Stream};

use crate::adapters::common::run_mode;
use crate::{PyElement, pyadapter};

/// A lifecycle transition erases to a string, not a `#[pyclass]` enum.
impl From<AeronStatus> for PyElement {
    fn from(status: AeronStatus) -> Self {
        let name = match status {
            AeronStatus::Connected => "connected",
            AeronStatus::Disconnected => "disconnected",
            AeronStatus::BackPressured => "back_pressured",
            AeronStatus::Closed => "closed",
            // `AeronStatus` is `#[non_exhaustive]`: a variant added upstream
            // must not silently become one of the four above.
            ref other => return PyElement::from(format!("unknown:{other:?}")),
        };
        PyElement::from(name)
    }
}

/// The polling-mode selector, a string rather than a `#[pyclass]` enum.
fn poll_mode(name: &str) -> Result<AeronMode> {
    match name {
        "spin" => Ok(AeronMode::Spin),
        "threaded" => Ok(AeronMode::Threaded),
        other => bail!("aeron: unknown mode '{other}'; expected 'spin' or 'threaded'"),
    }
}

/// The subscriber options a Python caller can set.
fn sub_options(mode: &str, fragment_limit: usize) -> Result<AeronSubOptions> {
    if fragment_limit == 0 {
        bail!("aeron: fragment_limit must be at least 1");
    }
    Ok(AeronSubOptions::default()
        .with_mode(poll_mode(mode)?)
        .with_fragment_limit(fragment_limit))
}

/// Reject a historical wiring **before** touching the media driver.
///
/// The engine rejects it too, but only after the subscription is resolved —
/// which for a Python caller means a pointless connect, and a driver error
/// where the real problem is the run mode. Checking here keeps the message
/// about the actual mistake.
fn reject_historical(realtime: bool, who: &str) -> Result<()> {
    if !realtime {
        bail!(
            "{who}: RunMode::HistoricalFrom is unsupported — an Aeron subscription is a \
             live, unbounded source with no historical timeline to replay; wire it with \
             realtime=True and run the graph with realtime=True"
        );
    }
    Ok(())
}

/// Copy each fragment out whole — the raw-`bytes` wire format.
fn whole_fragment(f: &FragmentBuffer<'_>) -> Result<Option<Vec<u8>>, TransportError> {
    Ok(Some(f.as_ref().to_vec()))
}

/// Resolve a subscription through the media driver, at wiring time.
///
/// Takes an already-validated `timeout` so every argument check runs *before*
/// the driver is contacted — a bad argument must not cost a connect attempt
/// (or report itself as a driver timeout).
fn subscribe(
    channel: &str,
    stream_id: i32,
    timeout: Duration,
    fragment_limit: usize,
) -> Result<impl wingfoil::adapters::aeron::AeronSubscriberBackend> {
    Ok(AeronHandle::connect()?
        .subscription(channel, stream_id, timeout)?
        .with_fragment_limit(fragment_limit))
}

/// Resolve a publication through the media driver, at wiring time.
fn publish(
    channel: &str,
    stream_id: i32,
    timeout: Duration,
) -> Result<impl wingfoil::adapters::aeron::AeronPublisherBackend> {
    AeronHandle::connect()?.publication(channel, stream_id, timeout)
}

/// The resolve timeout, rejecting a value `Duration` cannot represent.
fn timeout(secs: f64) -> Result<Duration> {
    if !secs.is_finite() || secs <= 0.0 {
        bail!("aeron: timeout_secs must be a finite, positive number, got {secs}");
    }
    Ok(Duration::from_secs_f64(secs))
}

/// Subscribe to an Aeron channel.
///
/// Each tick yields a `list` of `bytes` — the fragments received in that graph
/// cycle, losslessly grouped. A malformed fragment is logged and skipped rather
/// than aborting the cycle.
///
/// `mode` is `"spin"` (poll inside the graph cycle — lowest latency, one core)
/// or `"threaded"` (poll on a background thread, one channel hop of latency).
/// `fragment_limit` caps the fragments taken per poll.
///
/// `realtime` declares the run mode this source is wired for and must match the
/// eventual `graph.run(realtime=…)`; a subscription is live and unbounded with
/// no historical timeline, so `realtime=False` raises here.
///
/// Requires a running media driver: the subscription is resolved **here**, so
/// an unreachable driver or a subscription that does not resolve within
/// `timeout_secs` raises at wiring.
#[pyadapter(name = aeron_sub, source)]
#[pyo3(signature = (
    channel, stream_id, realtime = true, mode = "spin".to_string(),
    fragment_limit = DEFAULT_FRAGMENT_LIMIT, timeout_secs = 5.0,
))]
fn sub(
    g: &GraphBuilder,
    channel: String,
    stream_id: i32,
    realtime: bool,
    mode: String,
    fragment_limit: usize,
    timeout_secs: f64,
) -> Result<Stream<Burst<Vec<u8>>>> {
    reject_historical(realtime, "aeron_sub")?;
    let opts = sub_options(&mode, fragment_limit)?;
    let subscriber = subscribe(&channel, stream_id, timeout(timeout_secs)?, fragment_limit)?;
    aeron_sub_fragment(g, run_mode(realtime), subscriber, whole_fragment, opts)
}

/// [`aeron_sub`](sub), paired with a status stream.
///
/// Returns a **`(data, status)` tuple**: `data` is exactly [`aeron_sub`](sub)'s
/// output, and `status` ticks with a `list` of transition strings —
/// `"connected"`, `"disconnected"`, `"back_pressured"` or `"closed"` — **only
/// on cycles where the state changed**, never re-emitting in steady state.
///
/// ```python
/// data, status = aeron_sub_with_status(g, "aeron:ipc", 1001)
/// ```
///
/// This has no legacy `wingfoil-python` equivalent.
#[pyadapter(name = aeron_sub_with_status, source)]
#[pyo3(signature = (
    channel, stream_id, realtime = true, mode = "spin".to_string(),
    fragment_limit = DEFAULT_FRAGMENT_LIMIT, timeout_secs = 5.0,
))]
fn sub_with_status(
    g: &GraphBuilder,
    channel: String,
    stream_id: i32,
    realtime: bool,
    mode: String,
    fragment_limit: usize,
    timeout_secs: f64,
) -> Result<(Stream<Burst<Vec<u8>>>, Stream<Burst<AeronStatus>>)> {
    reject_historical(realtime, "aeron_sub_with_status")?;
    let opts = sub_options(&mode, fragment_limit)?;
    let subscriber = subscribe(&channel, stream_id, timeout(timeout_secs)?, fragment_limit)?;
    aeron_sub_fragment_with_status(g, run_mode(realtime), subscriber, whole_fragment, opts)
}

/// Publish this stream on an Aeron channel.
///
/// Each tick's value is `bytes` for one message, or a `list`/`tuple` of `bytes`
/// to send several on that tick — legacy could only send one. A value that is
/// not bytes-like aborts the run rather than publishing an empty message.
///
/// Publishing is real-time only: a historical run aborts at graph `start()`.
/// The publication is resolved at wiring (see [`aeron_sub`](sub)). Returns a
/// terminal stream whose value is `None`.
#[pyadapter(name = aeron_pub)]
#[pyo3(signature = (channel, stream_id, timeout_secs = 5.0))]
fn publish_to(
    stream: &Stream<Burst<Vec<u8>>>,
    channel: String,
    stream_id: i32,
    timeout_secs: f64,
) -> Result<Stream<()>> {
    let publisher = publish(&channel, stream_id, timeout(timeout_secs)?)?;
    Ok(stream.aeron_pub(publisher, Clone::clone))
}

/// [`aeron_pub`](publish_to), paired with a status stream.
///
/// Returns a **`(sink, status)` tuple**. `status` ticks with a `list` of
/// transition strings only on cycles where the state changed: a back-pressured
/// offer records `"back_pressured"` and drops the rest of that burst, a
/// successful offer records `"connected"`, and `"closed"` is terminal.
///
/// This has no legacy `wingfoil-python` equivalent.
#[pyadapter(name = aeron_pub_with_status)]
#[pyo3(signature = (channel, stream_id, timeout_secs = 5.0))]
fn publish_to_with_status(
    stream: &Stream<Burst<Vec<u8>>>,
    channel: String,
    stream_id: i32,
    timeout_secs: f64,
) -> Result<(Stream<()>, Stream<Burst<AeronStatus>>)> {
    let publisher = publish(&channel, stream_id, timeout(timeout_secs)?)?;
    Ok(stream.aeron_pub_with_status(publisher, Clone::clone))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pyo3::prelude::*;

    #[test]
    fn a_status_erases_to_a_string() {
        Python::attach(|py| {
            let name = |s: AeronStatus| {
                PyElement::from(s)
                    .object()
                    .bind(py)
                    .extract::<String>()
                    .unwrap()
            };
            assert_eq!("connected", name(AeronStatus::Connected));
            assert_eq!("disconnected", name(AeronStatus::Disconnected));
            assert_eq!("back_pressured", name(AeronStatus::BackPressured));
            assert_eq!("closed", name(AeronStatus::Closed));
        });
    }

    #[test]
    fn the_mode_selector_is_a_string() {
        assert_eq!(AeronMode::Spin, poll_mode("spin").unwrap());
        assert_eq!(AeronMode::Threaded, poll_mode("threaded").unwrap());
        let err = poll_mode("polling").unwrap_err();
        assert!(err.to_string().contains("expected 'spin' or 'threaded'"));
    }

    #[test]
    fn the_options_carry_the_defaults_and_reject_a_zero_limit() {
        let opts = sub_options("spin", DEFAULT_FRAGMENT_LIMIT).unwrap();
        assert_eq!(AeronMode::Spin, opts.mode);
        assert_eq!(256, opts.fragment_limit);
        assert!(sub_options("spin", 0).is_err());
    }

    #[test]
    fn a_historical_wiring_is_rejected_before_the_driver_is_touched() {
        assert!(reject_historical(true, "aeron_sub").is_ok());
        let err = reject_historical(false, "aeron_sub").unwrap_err();
        assert!(err.to_string().contains("HistoricalFrom"), "{err}");
    }

    #[test]
    fn the_timeout_rejects_a_nonsense_value() {
        assert_eq!(Duration::from_millis(1500), timeout(1.5).unwrap());
        assert!(timeout(0.0).is_err());
        assert!(timeout(-1.0).is_err());
        assert!(timeout(f64::NAN).is_err());
        assert!(timeout(f64::INFINITY).is_err());
    }

    #[test]
    fn a_fragment_is_copied_out_whole() {
        // The parser is what makes the wire format raw bytes; it never skips
        // and never errors, so every fragment reaches the burst.
        let bytes = [1_u8, 2, 3];
        let buffer = FragmentBuffer::new(
            &bytes,
            wingfoil::adapters::aeron::FragmentHeader {
                position: 0,
                session_id: 1,
                stream_id: 1001,
            },
        );
        assert_eq!(Some(bytes.to_vec()), whole_fragment(&buffer).unwrap());
    }
}
