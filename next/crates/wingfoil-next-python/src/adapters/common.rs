//! Shared helpers for the adapter bindings — the run-shape plumbing every
//! source binding needs, and the record-dict reader every sink binding needs.
//!
//! A Rust caller picks the run mode at `run()`, but several adapters need it (or
//! the whole run window) at **wiring** time: a time-sliced reader slices its
//! queries up front, and a live-tail source rejects a historical run. A Python
//! `Graph` does not know its run mode until `run()` either, so those bindings
//! take the run shape as arguments and rebuild it here — which is the same three
//! conversions in every one of them.
//!
//! On the way *out*, a sink binding marshals a Python `dict` into the adapter's
//! record type. That is the same handful of field reads every time — a required
//! payload, an optional key, a name with a caller-supplied fallback — each of
//! which must fail loudly rather than default. [`RecordDict`] is that reader.

use anyhow::{Result, anyhow, bail};
use pyo3::prelude::*;
use pyo3::types::PyDict;
use std::time::Duration;
use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::async_source::RunParams;

/// The [`RunParams`] a **historical** source is wired for, from the Python
/// `start_nanos` / `duration_nanos` arguments.
///
/// `RunFor::Duration` is the only bound a time-sliced reader can slice (it needs
/// a concrete end time), so this always builds one. The values must match the
/// eventual `graph.run(start_nanos=…, duration_nanos=…)`; a reader validates the
/// window at wiring, so a mismatched or empty one is rejected there.
pub fn historical_params(start_nanos: u64, duration_nanos: u64) -> RunParams {
    let start = NanoTime::from(start_nanos);
    RunParams {
        run_mode: RunMode::HistoricalFrom(start),
        run_for: RunFor::Duration(Duration::from_nanos(duration_nanos)),
        start_time: start,
    }
}

/// The [`RunParams`] a **real-time** source is wired for. Unbounded: a live
/// source's bound comes from the actual `run()`, not from wiring.
pub fn realtime_params() -> RunParams {
    RunParams {
        run_mode: RunMode::RealTime,
        run_for: RunFor::Forever,
        start_time: NanoTime::ZERO,
    }
}

/// The [`RunMode`] a source is being wired for, from the Python `realtime` flag.
///
/// The historical arm carries `ZERO`: a source taking only a mode (rather than
/// full params) uses it to *reject* the wrong mode, never to read the instant.
pub fn run_mode(realtime: bool) -> RunMode {
    if realtime {
        RunMode::RealTime
    } else {
        RunMode::HistoricalFrom(NanoTime::ZERO)
    }
}

/// Unix seconds -> [`NanoTime`], for the cursor/offset arguments adapters take
/// as a Python `float`. Rejects a negative or non-finite input rather than
/// wrapping it into a nonsense instant.
pub fn secs_to_nanotime(secs: f64) -> Result<NanoTime> {
    if !secs.is_finite() || secs < 0.0 {
        bail!("expected a finite, non-negative Unix-seconds value, got {secs}");
    }
    Ok(NanoTime::new((secs * 1e9) as u64))
}

/// A Python record `dict` on its way into an adapter's record type.
///
/// Wraps the dict with the binding's name so every failure reads
/// `"<binding>: <field>: <why>"` without each call site repeating it. Every
/// accessor **fails loudly** — a wrong-typed field is an error, never a
/// silently-dropped or defaulted value (the legacy bindings' `.ok().flatten()
/// .and_then(…).unwrap_or_default()` chains turned each of these into an empty
/// payload or a dropped key).
///
/// Throughout, a field that is *absent* and one explicitly set to Python `None`
/// are treated the same: both mean "not supplied".
pub struct RecordDict<'a, 'py> {
    dict: &'a Bound<'py, PyDict>,
    /// The binding name, e.g. `"kafka_pub"` — the error-message prefix.
    who: &'static str,
}

impl<'a, 'py> RecordDict<'a, 'py> {
    /// Read `dict` on behalf of the binding named `who`.
    pub fn new(dict: &'a Bound<'py, PyDict>, who: &'static str) -> Self {
        Self { dict, who }
    }

    /// The raw field, or `None` when absent or explicitly `None`.
    pub fn get(&self, name: &str) -> Result<Option<Bound<'py, PyAny>>> {
        let who = self.who;
        Ok(self
            .dict
            .get_item(name)
            .map_err(|e| anyhow!("{who}: reading key '{name}': {e}"))?
            .filter(|v| !v.is_none()))
    }

    /// A `str` field, falling back to `default` when not supplied.
    ///
    /// This is the "record names its own target, with a binding-level default"
    /// shape — a Kafka topic, a Redis channel or stream key. Not supplied *and*
    /// no default is an error naming the argument that would have provided one.
    pub fn str_or(&self, name: &str, default: Option<&str>, arg: &str) -> Result<String> {
        let who = self.who;
        match self.get(name)? {
            Some(v) => v
                .extract::<String>()
                .map_err(|e| anyhow!("{who}: '{name}' must be a str: {e}")),
            None => Ok(default
                .ok_or_else(|| {
                    anyhow!(
                        "{who}: record has no '{name}' and no default was given \
                         (pass {arg}=… to {who})"
                    )
                })?
                .to_string()),
        }
    }

    /// A **required** `str` field — the shape for a record whose target has no
    /// binding-level fallback (an etcd key, say), as opposed to
    /// [`str_or`](Self::str_or).
    pub fn str(&self, name: &str) -> Result<String> {
        let who = self.who;
        self.get(name)?
            .ok_or_else(|| anyhow!("{who}: record is missing '{name}'"))?
            .extract::<String>()
            .map_err(|e| anyhow!("{who}: '{name}' must be a str: {e}"))
    }

    /// A required bytes-like field (`bytes` / `bytearray`, or a sequence of
    /// ints — whatever pyo3 will extract).
    pub fn bytes(&self, name: &str) -> Result<Vec<u8>> {
        let who = self.who;
        self.get(name)?
            .ok_or_else(|| anyhow!("{who}: record is missing '{name}'"))?
            .extract::<Vec<u8>>()
            .map_err(|e| anyhow!("{who}: '{name}' must be bytes-like: {e}"))
    }

    /// An optional bytes-like field — `None` when not supplied.
    pub fn opt_bytes(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let who = self.who;
        match self.get(name)? {
            Some(v) => v
                .extract::<Vec<u8>>()
                .map(Some)
                .map_err(|e| anyhow!("{who}: '{name}' must be bytes-like: {e}")),
            None => Ok(None),
        }
    }

    /// A required nested `dict` of `str -> bytes`, in iteration order — the
    /// shape of a Redis stream entry's fields.
    pub fn byte_fields(&self, name: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let who = self.who;
        let value = self
            .get(name)?
            .ok_or_else(|| anyhow!("{who}: record is missing '{name}'"))?;
        let fields = value
            .cast::<PyDict>()
            .map_err(|_| anyhow!("{who}: '{name}' must be a dict of str -> bytes"))?;
        fields
            .iter()
            .map(|(key, value)| {
                let key = key
                    .extract::<String>()
                    .map_err(|e| anyhow!("{who}: '{name}' keys must be str: {e}"))?;
                let value = value.extract::<Vec<u8>>().map_err(|e| {
                    anyhow!("{who}: '{name}' value for '{key}' must be bytes-like: {e}")
                })?;
                Ok((key, value))
            })
            .collect()
    }
}

/// Cast one erased stream value to the record `dict` a sink binding expects.
///
/// The empty-element and non-dict cases are the same two errors in every sink,
/// and both are ones the legacy bindings swallowed — a non-dict value logged and
/// produced an *empty burst*, writing nothing at all.
pub fn record_dict<'py>(
    elem: &crate::PyElement,
    py: Python<'py>,
    who: &'static str,
    shape: &str,
) -> Result<Bound<'py, PyDict>> {
    if elem.is_none() {
        bail!("{who}: stream value is empty (the upstream produced no value)");
    }
    let obj = elem.object().bind(py);
    obj.cast::<PyDict>().cloned().map_err(|_| {
        anyhow!(
            "{who}: stream value must be a dict {shape}, or a list of such dicts, got {}",
            obj.get_type()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a record dict from `(key, value)` pairs.
    fn dict<'py>(py: Python<'py>, items: &[(&str, Py<PyAny>)]) -> Bound<'py, PyDict> {
        let d = PyDict::new(py);
        for (k, v) in items {
            d.set_item(k, v).unwrap();
        }
        d
    }

    fn bytes_obj(py: Python<'_>, b: &[u8]) -> Py<PyAny> {
        pyo3::types::PyBytes::new(py, b).into_any().unbind()
    }

    fn str_obj(py: Python<'_>, s: &str) -> Py<PyAny> {
        s.into_pyobject(py).unwrap().into_any().unbind()
    }

    #[test]
    fn str_or_prefers_the_field_over_the_default() {
        Python::attach(|py| {
            let d = dict(py, &[("topic", str_obj(py, "own"))]);
            let rec = RecordDict::new(&d, "kafka_pub");
            assert_eq!(
                "own",
                rec.str_or("topic", Some("fallback"), "topic").unwrap()
            );
        });
    }

    #[test]
    fn str_or_falls_back_when_absent_or_none() {
        Python::attach(|py| {
            let absent = dict(py, &[]);
            let rec = RecordDict::new(&absent, "kafka_pub");
            assert_eq!("fb", rec.str_or("topic", Some("fb"), "topic").unwrap());

            let explicit_none = dict(py, &[("topic", py.None())]);
            let rec = RecordDict::new(&explicit_none, "kafka_pub");
            assert_eq!("fb", rec.str_or("topic", Some("fb"), "topic").unwrap());
        });
    }

    #[test]
    fn str_or_without_a_default_names_the_argument() {
        Python::attach(|py| {
            let d = dict(py, &[]);
            let rec = RecordDict::new(&d, "kafka_pub");
            let err = rec.str_or("topic", None, "topic").unwrap_err();
            assert!(err.to_string().contains("no 'topic'"), "got: {err}");
            assert!(err.to_string().contains("pass topic="), "got: {err}");
        });
    }

    #[test]
    fn a_wrong_typed_str_errors_rather_than_falling_back() {
        Python::attach(|py| {
            let d = dict(
                py,
                &[(
                    "topic",
                    7_i64.into_pyobject(py).unwrap().unbind().into_any(),
                )],
            );
            let rec = RecordDict::new(&d, "kafka_pub");
            assert!(rec.str_or("topic", Some("fb"), "topic").is_err());
        });
    }

    #[test]
    fn bytes_is_required_and_type_checked() {
        Python::attach(|py| {
            let ok = dict(py, &[("value", bytes_obj(py, b"v"))]);
            assert_eq!(
                b"v".to_vec(),
                RecordDict::new(&ok, "kafka_pub").bytes("value").unwrap()
            );

            let missing = dict(py, &[]);
            let err = RecordDict::new(&missing, "kafka_pub")
                .bytes("value")
                .unwrap_err();
            assert!(err.to_string().contains("missing 'value'"), "got: {err}");

            let wrong = dict(
                py,
                &[(
                    "value",
                    1.5_f64.into_pyobject(py).unwrap().into_any().unbind(),
                )],
            );
            assert!(RecordDict::new(&wrong, "kafka_pub").bytes("value").is_err());
        });
    }

    #[test]
    fn opt_bytes_treats_absent_and_none_alike() {
        Python::attach(|py| {
            let absent = dict(py, &[]);
            assert_eq!(
                None,
                RecordDict::new(&absent, "kafka_pub")
                    .opt_bytes("key")
                    .unwrap()
            );

            let explicit = dict(py, &[("key", py.None())]);
            assert_eq!(
                None,
                RecordDict::new(&explicit, "kafka_pub")
                    .opt_bytes("key")
                    .unwrap()
            );

            let present = dict(py, &[("key", bytes_obj(py, b"k"))]);
            assert_eq!(
                Some(b"k".to_vec()),
                RecordDict::new(&present, "kafka_pub")
                    .opt_bytes("key")
                    .unwrap()
            );
        });
    }

    #[test]
    fn a_wrong_typed_optional_field_errors_rather_than_being_dropped() {
        Python::attach(|py| {
            let d = dict(
                py,
                &[("key", 7_i64.into_pyobject(py).unwrap().into_any().unbind())],
            );
            assert!(RecordDict::new(&d, "kafka_pub").opt_bytes("key").is_err());
        });
    }

    #[test]
    fn byte_fields_reads_a_nested_str_to_bytes_dict() {
        Python::attach(|py| {
            let inner = dict(
                py,
                &[("a", bytes_obj(py, b"1")), ("b", bytes_obj(py, b"2"))],
            );
            let outer = dict(py, &[("fields", inner.into_any().unbind())]);
            let fields = RecordDict::new(&outer, "redis_stream_write")
                .byte_fields("fields")
                .unwrap();
            assert_eq!(
                vec![
                    ("a".to_string(), b"1".to_vec()),
                    ("b".to_string(), b"2".to_vec())
                ],
                fields
            );
        });
    }

    #[test]
    fn byte_fields_rejects_a_non_dict_and_a_wrong_typed_value() {
        Python::attach(|py| {
            let not_a_dict = dict(py, &[("fields", str_obj(py, "nope"))]);
            assert!(
                RecordDict::new(&not_a_dict, "redis_stream_write")
                    .byte_fields("fields")
                    .is_err()
            );

            let inner = dict(py, &[("a", str_obj(py, "not-bytes"))]);
            let outer = dict(py, &[("fields", inner.into_any().unbind())]);
            let err = RecordDict::new(&outer, "redis_stream_write")
                .byte_fields("fields")
                .unwrap_err();
            assert!(err.to_string().contains("for 'a'"), "got: {err}");
        });
    }

    #[test]
    fn historical_params_span_the_requested_window() {
        let params = historical_params(1_000, 500);
        assert!(matches!(
            params.run_mode,
            RunMode::HistoricalFrom(t) if t == NanoTime::from(1_000_u64)
        ));
        assert!(matches!(params.run_for, RunFor::Duration(d) if d.as_nanos() == 500));
        assert_eq!(NanoTime::from(1_000_u64), params.start_time);
    }

    #[test]
    fn realtime_params_are_unbounded() {
        let params = realtime_params();
        assert!(matches!(params.run_mode, RunMode::RealTime));
        assert!(matches!(params.run_for, RunFor::Forever));
    }

    #[test]
    fn run_mode_maps_the_realtime_flag() {
        assert!(matches!(run_mode(true), RunMode::RealTime));
        assert!(matches!(run_mode(false), RunMode::HistoricalFrom(_)));
    }

    #[test]
    fn secs_to_nanotime_rejects_nonsense() {
        assert!(secs_to_nanotime(-1.0).is_err());
        assert!(secs_to_nanotime(f64::NAN).is_err());
        assert!(secs_to_nanotime(f64::INFINITY).is_err());
        assert_eq!(NanoTime::new(1_500_000_000), secs_to_nanotime(1.5).unwrap());
        assert_eq!(NanoTime::ZERO, secs_to_nanotime(0.0).unwrap());
    }
}
