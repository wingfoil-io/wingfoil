//! Python bindings for the wingfoil **iceoryx2** adapter
//! ([`wingfoil::adapters::iceoryx2`]).
//!
//! Two graph entry points, both `#[pyadapter]`-generated:
//!
//! | Python                     | Rust                                          | shape |
//! |----------------------------|-----------------------------------------------|-------|
//! | `iceoryx2_sub(graph, …)`   | [`iceoryx2_sub_slice_opts`]                   | service → `list[bytes]` (or `list[TracedBytes]`) |
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
//! # The `stages` latency-tracing path
//!
//! Both entry points take an optional `stages` list, which turns the samples
//! into *traced* ones: the wire frame becomes a `[u64; len(stages)]`
//! little-endian stamp header followed by the payload bytes, and the Python
//! value becomes a [`TracedBytes`](crate::latency::PyTracedBytes) carrying a
//! [`Latency`](crate::latency::PyLatency) record instead of plain `bytes`. So a
//! subscriber can `stamp` a hop it received and a publisher ships the record
//! alongside the payload:
//!
//! ```python
//! stages = ["send", "recv", "decode"]
//! wf.iceoryx2_pub(traced_stream, "svc", stages=stages)                 # header ++ payload
//! received = wf.iceoryx2_sub(g, "svc", stages=stages)                  # list[TracedBytes]
//! sink, stats = wf.latency_report(wf.stamp(received, "recv"), stages)
//! ```
//!
//! Nothing here re-implements the record: the split and the pack are
//! [`PyLatency::create_from_bytes`] / [`PyLatency::header_bytes`], and the stage
//! list is validated at wiring by [`check_stages`] — the same rules
//! `Latency(stages)` itself enforces. The header layout is the one a Rust peer's
//! [`latency_stages!`](wingfoil::latency::latency_stages) record has in
//! memory, so a Python subscriber reads a Rust publisher's stamps directly.
//!
//! Both directions marshal on the **graph thread** under one
//! [`Python::attach`] per burst: the adapter's own worker (in `"threaded"` /
//! `"signaled"` mode) only ever carries `Vec<u8>`, never a Python object.
//!
//! Because a `#[pyadapter]` fn has one return type, the *decode* is a node of
//! this module rather than the erasure seam — the subscriber hands back
//! `Stream<Burst<PyElement>>` either way and the seam only groups it into a
//! `list`. That is deliberate: the untraced path costs one extra `Vec` per
//! tick, not an extra GIL acquire (the seam's attach nests inside this one).
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
//! 3. **Publishing rejects a non-bytes value loudly**, naming the type it got
//!    and aborting the run — legacy hand-rolled the same check in the binding.
//! 4. **A `stages` frame too short for its header aborts the run**, naming both
//!    lengths. Legacy logged a warning and handed the *whole* frame back as the
//!    payload with an all-zero record, so a contract mismatch between publisher
//!    and subscriber surfaced as silently corrupt payloads. This is the same
//!    rejection `Latency.from_bytes` already makes on a short buffer.
//! 5. **Publishing with `stages` checks the record's stage list**, and aborts
//!    naming both lists when it differs from the one wired. The header is
//!    positional, so a record over different stages produces a frame the
//!    subscriber decodes into the wrong names; legacy packed whatever it was
//!    given.
//! 6. **`stages` publishes bursts too.** A tick carrying a `list`/`tuple` of
//!    `TracedBytes` sends one traced sample per member, as the untraced path
//!    already did; legacy's instrumented path accepted only a single value.

use anyhow::{Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use wingfoil::adapters::iceoryx2::{
    ICEORYX2_DEFAULT_HISTORY_SIZE, ICEORYX2_DEFAULT_INITIAL_MAX_SLICE_LEN, Iceoryx2Mode,
    Iceoryx2PubSliceOpts, Iceoryx2ServiceVariant, Iceoryx2SliceSinkOps, Iceoryx2SubOpts,
    iceoryx2_sub_slice_opts,
};
use wingfoil::prelude::{Burst, GraphBuilder, Stream, StreamOps};

use crate::PyElement;
use crate::adapters::common::run_mode;
use crate::latency::{PyLatency, PyTracedBytes, STAMP_BYTES, check_stages};
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

/// Validate a `stages` argument at wiring, by the same rules `Latency(stages)`
/// applies — an empty or duplicate-carrying list can never address its stamps.
fn checked_stages(who: &str, stages: Option<Vec<String>>) -> Result<Option<Vec<String>>> {
    match stages {
        None => Ok(None),
        Some(stages) => {
            check_stages(&stages).map_err(|err| anyhow!("{who}: {err}"))?;
            Ok(Some(stages))
        }
    }
}

/// Decode one burst of received frames into plain `bytes` values.
fn decode_bytes(burst: &Burst<Vec<u8>>) -> Burst<PyElement> {
    Python::attach(|_py| burst.iter().map(|b| PyElement::from(b.clone())).collect())
}

/// Decode one burst of received frames into `TracedBytes` values: the leading
/// `8 * stages.len()` bytes are the stamp header, the rest is the payload.
fn decode_traced(burst: &Burst<Vec<u8>>, stages: &[String]) -> Result<Burst<PyElement>> {
    let header_len = stages.len() * STAMP_BYTES;
    Python::attach(|py| {
        let mut out = Burst::default();
        for frame in burst.iter() {
            if frame.len() < header_len {
                bail!(
                    "iceoryx2_sub(stages): a {}-byte sample is too short for the {header_len}-byte \
                     header of {} stages {stages:?} — the publisher's stages must match",
                    frame.len(),
                    stages.len(),
                );
            }
            let latency = PyLatency::create_from_bytes(&frame[..header_len], stages.to_vec());
            let traced = PyTracedBytes {
                payload: PyBytes::new(py, &frame[header_len..]).into_any().unbind(),
                latency: Py::new(py, latency)?,
            };
            out.push(PyElement::new(Py::new(py, traced)?.into_any()));
        }
        Ok(out)
    })
}

/// Encode one burst of Python values as plain payload frames.
fn encode_bytes(burst: &Burst<PyElement>) -> Result<Burst<Vec<u8>>> {
    Python::attach(|_py| {
        burst
            .iter()
            .map(|value| {
                Vec::<u8>::try_from(value)
                    .map_err(|err| anyhow!("iceoryx2_pub: expected bytes: {err}"))
            })
            .collect()
    })
}

/// Encode one burst of `TracedBytes` values as `header ++ payload` frames.
fn encode_traced(burst: &Burst<PyElement>, stages: &[String]) -> Result<Burst<Vec<u8>>> {
    Python::attach(|py| {
        let mut out = Burst::default();
        for value in burst.iter() {
            let obj = value.object().bind(py);
            let traced = obj.extract::<PyRef<'_, PyTracedBytes>>().map_err(|_| {
                let got = obj
                    .get_type()
                    .qualname()
                    .map(|name| name.to_string())
                    .unwrap_or_else(|_| "?".to_string());
                anyhow!("iceoryx2_pub(stages): expected a TracedBytes, got {got}")
            })?;
            let record = traced.latency.borrow(py);
            if record.stages_ref() != stages {
                bail!(
                    "iceoryx2_pub(stages): wired for stages {stages:?} but the record carries {:?}",
                    record.stages_ref(),
                );
            }
            let payload = traced
                .payload
                .bind(py)
                .extract::<Vec<u8>>()
                .map_err(|err| anyhow!("iceoryx2_pub(stages): TracedBytes.payload: {err}"))?;
            let mut frame = record.header_bytes();
            frame.extend_from_slice(&payload);
            out.push(frame);
        }
        Ok(out)
    })
}

/// Subscribe to an iceoryx2 byte-slice service.
///
/// Each tick yields a `list` of the samples received between graph cycles,
/// losslessly grouped — `bytes` each, or `TracedBytes` when `stages` is given.
///
/// `variant` is `"ipc"` (shared memory, across processes) or `"local"`
/// (in-process, over the heap). `mode` is `"spin"` (poll inside the graph
/// cycle — lowest latency, one core), `"threaded"` (a background thread, one
/// channel hop) or `"signaled"` (a blocking `WaitSet`, which needs the
/// publisher to signal on the matching Event service). `history_size` is how
/// many past samples a late joiner receives, and is part of the **service
/// contract**: every participant on the same service must agree.
///
/// `stages` names the latency stages each sample carries: the frame's leading
/// `8 * len(stages)` bytes are read as the stamp header and the rest becomes the
/// payload, so the tick's values are `TracedBytes` ready for `stamp` /
/// `latency_report`. It is part of the wire contract too — the publisher must
/// send the same list, and a sample too short for the header aborts the run.
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
    stages = None,
))]
fn sub(
    g: &GraphBuilder,
    service_name: String,
    realtime: bool,
    variant: String,
    mode: String,
    history_size: usize,
    stages: Option<Vec<String>>,
) -> Result<Stream<Burst<PyElement>>> {
    let opts = Iceoryx2SubOpts::default()
        .with_variant(self::variant(&variant)?)
        .with_mode(poll_mode(&mode)?)
        .with_history_size(history_size);
    let stages = checked_stages("iceoryx2_sub", stages)?;
    let frames = iceoryx2_sub_slice_opts(g, run_mode(realtime), &service_name, opts)?;
    // The decode is a node of ours rather than the `#[pyadapter]` erasure seam,
    // because the two paths produce different Python values from the same
    // `Burst<Vec<u8>>`; the seam then erases the burst of already-Python values
    // to a `list` under the same (nested, therefore cheap) attach.
    Ok(match stages {
        None => frames.map(decode_bytes),
        Some(stages) => frames.try_map(move |burst: &Burst<Vec<u8>>| decode_traced(burst, &stages)),
    })
}

/// Publish this stream to an iceoryx2 byte-slice service.
///
/// Each tick's value is one sample, or a `list`/`tuple` to send several on that
/// tick. A sample is `bytes`, or a `TracedBytes` when `stages` is given — in
/// which case the frame sent is the record's `8 * len(stages)`-byte stamp header
/// followed by the payload, which a subscriber wired with the same `stages`
/// reads straight back. Anything else aborts the run.
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
    stages = None,
))]
fn publish(
    stream: &Stream<Burst<PyElement>>,
    service_name: String,
    variant: String,
    history_size: usize,
    initial_max_slice_len: usize,
    stages: Option<Vec<String>>,
) -> Result<Stream<()>> {
    if initial_max_slice_len == 0 {
        bail!("iceoryx2_pub: initial_max_slice_len must be at least 1");
    }
    let opts = Iceoryx2PubSliceOpts::default()
        .with_variant(self::variant(&variant)?)
        .with_history_size(history_size)
        .with_initial_max_slice_len(initial_max_slice_len);
    // The input stays erased (rather than `Burst<Vec<u8>>` at the seam) so the
    // traced path can read a `TracedBytes` — `stages` is not in scope there.
    let frames = match checked_stages("iceoryx2_pub", stages)? {
        None => stream.try_map(encode_bytes),
        Some(stages) => {
            stream.try_map(move |burst: &Burst<PyElement>| encode_traced(burst, &stages))
        }
    };
    Ok(frames.iceoryx2_pub_slice_opts(&service_name, opts))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `TracedBytes(payload, Latency(stages))` with `stamps` written in, as a
    /// graph value — what a traced publisher's upstream ticks.
    fn traced_element(payload: &[u8], stage_names: &[&str], stamps: &[u64]) -> PyElement {
        let header: Vec<u8> = stamps.iter().flat_map(|v| v.to_le_bytes()).collect();
        Python::attach(|py| {
            let record = PyLatency::create_from_bytes(&header, stages(stage_names));
            let traced = PyTracedBytes {
                payload: PyBytes::new(py, payload).into_any().unbind(),
                latency: Py::new(py, record).expect("invariant: allocating a pyclass cannot fail"),
            };
            PyElement::new(
                Py::new(py, traced)
                    .expect("invariant: allocating a pyclass cannot fail")
                    .into_any(),
            )
        })
    }

    /// One erased value as a single-element burst — the shape the sink seam
    /// hands `encode_*` for a scalar tick.
    fn one(value: PyElement) -> Burst<PyElement> {
        let mut burst = Burst::default();
        burst.push(value);
        burst
    }

    /// The `(payload, stamps)` of each `TracedBytes` a decode produced.
    fn traced_contents(burst: &Burst<PyElement>) -> Vec<(Vec<u8>, Vec<u64>)> {
        Python::attach(|py| {
            burst
                .iter()
                .map(|value| {
                    let traced: PyRef<'_, PyTracedBytes> = value
                        .object()
                        .bind(py)
                        .extract()
                        .expect("invariant: the decode produced TracedBytes");
                    let payload = traced
                        .payload
                        .bind(py)
                        .extract::<Vec<u8>>()
                        .expect("invariant: the decode produced a bytes payload");
                    let record = traced.latency.borrow(py);
                    (payload, record.stamps())
                })
                .collect()
        })
    }

    fn stages(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

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

    #[test]
    fn an_unusable_stage_list_is_rejected_at_wiring() {
        assert!(checked_stages("iceoryx2_sub", None).unwrap().is_none());
        assert!(
            checked_stages("iceoryx2_sub", Some(stages(&["a", "b"])))
                .unwrap()
                .is_some()
        );
        for (bad, expected) in [
            (vec![], "stages list must not be empty"),
            (stages(&["a", "a"]), "duplicate stage name"),
        ] {
            let err = checked_stages("iceoryx2_sub", Some(bad)).unwrap_err();
            let msg = format!("{err:#}");
            assert!(
                msg.contains("iceoryx2_sub:") && msg.contains(expected),
                "{msg}"
            );
        }
    }

    #[test]
    fn a_traced_frame_round_trips_through_the_wire_form() {
        let names = stages(&["send", "recv"]);
        let frames = encode_traced(
            &one(traced_element(b"payload", &["send", "recv"], &[11, 22])),
            &names,
        )
        .unwrap();
        // 2 stages * 8 bytes of header, then the payload.
        assert_eq!(2 * STAMP_BYTES + b"payload".len(), frames[0].len());

        let decoded = decode_traced(&frames, &names).unwrap();
        assert_eq!(
            vec![(b"payload".to_vec(), vec![11, 22])],
            traced_contents(&decoded)
        );
    }

    #[test]
    fn an_untraced_frame_round_trips_as_bytes() {
        let frames = encode_bytes(&one(PyElement::from(b"raw".to_vec()))).unwrap();
        assert_eq!(vec![b"raw".to_vec()], frames.to_vec());
        let decoded = decode_bytes(&frames);
        assert_eq!(
            vec![b"raw".to_vec()],
            decoded
                .iter()
                .map(|v| Vec::<u8>::try_from(v).unwrap())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_burst_publishes_one_traced_sample_per_member() {
        let names = stages(&["send", "recv"]);
        let mut burst = Burst::default();
        burst.push(traced_element(b"a", &["send", "recv"], &[1, 2]));
        burst.push(traced_element(b"bb", &["send", "recv"], &[3, 4]));

        let frames = encode_traced(&burst, &names).unwrap();
        assert_eq!(2, frames.len());
        assert_eq!(
            vec![(b"a".to_vec(), vec![1, 2]), (b"bb".to_vec(), vec![3, 4])],
            traced_contents(&decode_traced(&frames, &names).unwrap())
        );
    }

    #[test]
    fn a_frame_too_short_for_its_header_is_rejected() {
        let mut burst = Burst::default();
        burst.push(b"tiny".to_vec());
        let err = decode_traced(&burst, &stages(&["send", "recv"])).unwrap_err();
        assert!(
            format!("{err:#}").contains("a 4-byte sample is too short for the 16-byte header"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn publishing_a_record_over_different_stages_is_rejected() {
        let err = encode_traced(
            &one(traced_element(b"payload", &["send", "decode"], &[1, 2])),
            &stages(&["send", "recv"]),
        )
        .unwrap_err();
        assert!(
            format!("{err:#}").contains("wired for stages [\"send\", \"recv\"] but the record"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn publishing_a_non_traced_value_with_stages_is_rejected() {
        let err = encode_traced(&one(PyElement::from(1.0_f64)), &stages(&["send"])).unwrap_err();
        assert!(
            format!("{err:#}").contains("expected a TracedBytes, got float"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn publishing_a_non_bytes_value_is_rejected() {
        let err = encode_bytes(&one(PyElement::from(1.0_f64))).unwrap_err();
        assert!(
            format!("{err:#}").contains("iceoryx2_pub: expected bytes"),
            "unexpected error: {err:#}"
        );
    }
}
