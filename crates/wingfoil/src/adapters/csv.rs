//! A CSV file adapter — a serde-typed replay **source** and a file **sink**,
//! the parsing cousin of the dependency-free [`lines`](crate::adapters::lines)
//! adapter (see `docs/port-plan.md`, Phase 4: "csv — replay source + sink;
//! exercises 0.3 historical bursts"). It ports the legacy
//! `wingfoil::adapters::csv` module onto the Op model.
//!
//! Records are ordinary Rust types that implement [`serde::Serialize`] /
//! [`serde::de::DeserializeOwned`] — a named struct, or a positional tuple such
//! as `(NanoTime, u32)`. The `csv` crate handles field mapping exactly as it
//! does for the legacy adapter.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`stats`](crate::stats)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — the free builder function [`csv_read`] on a
//!   [`GraphBuilder`]: deterministic historical replay of a CSV file, emitting
//!   `Stream<Burst<T>>`.
//! - **Sink** — the [`CsvSinkOps`] extension trait on `Stream<Burst<T>>` (and,
//!   for convenience, `Stream<T>` — each value auto-wrapped into a one-element
//!   burst), enabled with `use wingfoil::adapters::csv::CsvSinkOps;`.
//!
//! # Historical replay (the burst model)
//!
//! [`csv_read`] opens the file at wiring (fail-fast) and streams its rows
//! **lazily** over a [`produce_async`](crate::async_source::produce_async)
//! producer: each record is deserialized on demand, stamped with
//! `get_time(&record)`, and delivered on the graph clock. The channel receiver
//! groups records sharing a timestamp into one atomic [`Burst`](crate::Burst)
//! and replays them deterministically at their timestamps — lossless, in order,
//! independent of wall-clock. This mirrors the legacy `csv_read`, whose
//! `TryIteratorStream` groups same-timestamp rows into one burst (use
//! `.collapse_accumulate()` when the source is strictly ascending and you want a
//! flat `Vec<T>`).
//!
//! `buffer_size` bounds the replay's look-ahead as back-pressure: `Some(n)` keeps
//! at most ~`n` timestamp-groups queued ahead of the graph, so a huge file is
//! **not** read into memory up front — rows are pulled only as the graph drains
//! (a same-time group of any size rides one slot, never split). `None` is
//! unbounded. (Reads happen on the async producer task, so wiring stays
//! side-effect-free apart from the open; the file I/O itself is synchronous and
//! occupies one runtime worker during replay.)
//!
//! # Deviations from legacy
//!
//! - **Non-decreasing timestamps.** Records are delivered on the monotonic graph
//!   clock, so their timestamps must be **non-decreasing** (an out-of-order
//!   record aborts the run). Legacy's `TryIteratorStream` imposes no explicit
//!   ordering constraint at the source, though a backwards timestamp would fail
//!   the monotonic engine there too.
//! - **Eager header write.** [`csv_write`](CsvSinkOps::csv_write) opens the file
//!   and writes the CSV header at wiring time (before `for_each_mut`), whereas
//!   legacy defers the header to the first tick via a `headers_written` flag in
//!   `CsvWriterNode` (see `legacy/wingfoil/src/adapters/csv/write.rs`). Observable
//!   difference for a named-struct record: a graph that wires `csv_write` but
//!   produces zero rows leaves a header-only file in next, but an empty file in
//!   legacy (whose `cycle` never runs, so the header is never written). For a
//!   positional-tuple record no header is written in either, so there is no
//!   difference.
//!
//! # Sink
//!
//! [`CsvSinkOps::csv_write`] opens the target file once at wiring time, writes
//! the header up front (a leading `time` column plus the record's serde field
//! names, via `serde_aux` introspection — a positional tuple record has no
//! named fields, so no header is written, matching legacy), and returns a
//! `Stream<()>` sink built on
//! [`with_time`](crate::fluent::StreamOps::with_time) then the shared
//! [`for_each_mut`](crate::fluent::StreamOps::for_each_mut): each cycle
//! serializes every record in the incoming burst as one `(time, record)` row
//! and flushes, with any I/O or serialization error propagated through the
//! fallible cycle (`?` and `.context`).

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_aux::serde_introspection::serde_introspect;
use wingfoil::NanoTime;

use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, Stream, StreamOps};
use crate::{Burst, burst};

/// Deterministic historical replay of a CSV file: each record is emitted as a
/// [`Burst<T>`] on the graph clock at `get_time(&record)`, with records sharing
/// a timestamp grouped into one atomic burst.
///
/// The file is opened at wiring (fail-fast) and its rows are streamed **lazily**
/// over a [`produce_async`](crate::async_source::produce_async)
/// producer — deserialized on demand, not read into memory up front. Run the
/// graph with `RunMode::HistoricalFrom(t)` where `t` is at or before the first
/// record's timestamp (e.g. `NanoTime::ZERO`). Use
/// [`collapse_accumulate`](crate::fluent::Stream::collapse_accumulate) when the
/// source is strictly ascending and you want a flat `Vec<T>`.
///
/// `has_headers` toggles whether the first line is a header (skipped) or data,
/// exactly as `csv::ReaderBuilder::has_headers`.
///
/// `buffer_size` bounds the replay's look-ahead as back-pressure: `Some(n)` caps
/// the queued backlog to ~`n` timestamp-groups (a huge file stays bounded in
/// memory — rows are pulled only as the graph drains); `None` is unbounded.
///
/// # Errors
///
/// Returns an error at **wiring time** if the file cannot be opened. A row that
/// fails to deserialize does not panic — it is propagated into the graph and
/// surfaces as a run failure with a "failed to deserialize row" context (the same
/// outcome as the legacy adapter), now surfaced **mid-stream** as the reader
/// reaches it (like legacy), not up front. Record timestamps must be
/// non-decreasing; an out-of-order record fails the run.
pub fn csv_read<T, F>(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    get_time: F,
    has_headers: bool,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + DeserializeOwned + Send + 'static,
    F: Fn(&T) -> NanoTime + Send + 'static,
{
    let path = path.as_ref();
    let file =
        File::open(path).with_context(|| format!("csv_read: failed to open {}", path.display()))?;
    let display = path.display().to_string();
    // `into_deserialize` owns the reader, so the iterator is `Send + 'static`; it
    // is lazy (reads nothing until polled), so building it at wiring does no I/O
    // beyond the open above. The rows are pulled on the async producer task as the
    // graph drains, deserialized on demand and delivered at their timestamps — so
    // a malformed row aborts the run mid-stream (legacy's abort-not-panic), and a
    // huge file never lands in memory at once.
    let mut iter = csv::ReaderBuilder::new()
        .has_headers(has_headers)
        .from_reader(file)
        .into_deserialize::<T>();
    produce_async(
        g,
        move |_p: RunParams| async move {
            Ok(async_stream::stream! {
                for record in iter.by_ref() {
                    match record {
                        Ok(rec) => {
                            let time = get_time(&rec);
                            yield Ok((time, rec));
                        }
                        Err(e) => {
                            yield Err(anyhow::Error::new(e).context(format!(
                                "csv_read: failed to deserialize row from {display}"
                            )));
                            break;
                        }
                    }
                }
            })
        },
        buffer_size,
    )
}

/// A CSV file sink — the outbound counterpart of [`csv_read`].
///
/// An extension trait on `Stream<Burst<T>>` — and, for convenience,
/// `Stream<T>` (each value auto-wrapped into a one-element burst) — so `use`ing
/// it enables `stream.csv_write(path)` chaining, layered over the existing
/// [`with_time`](crate::fluent::StreamOps::with_time) +
/// [`for_each_mut`](crate::fluent::StreamOps::for_each_mut) ops the same way
/// [`LinesSinkOps`](crate::adapters::lines::LinesSinkOps) layers over
/// `for_each_mut`. Each emitted burst writes every record as one `(time,
/// record)` row — a leading `time` column carrying the graph time — then
/// flushes; an I/O or serialization error aborts the run with context.
pub trait CsvSinkOps<T> {
    /// Write each record to `path` as a `(time, record)` CSV row, **truncating**
    /// the file first (created if absent). The header (a `time` column plus the
    /// record's serde field names) is written up front unless the record is a
    /// positional tuple with no named fields. Returns the sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened (or its header cannot be
    /// written) at wiring time. A per-row I/O or serialization failure aborts
    /// the run with context.
    fn csv_write(&self, path: impl AsRef<Path>) -> Result<Stream<()>>;

    /// Like [`csv_write`](Self::csv_write), but with the column names supplied
    /// **explicitly** rather than derived from `T`'s serde field names.
    ///
    /// [`csv_write`](Self::csv_write) reads its header off the record type, so a
    /// record whose shape is only known at runtime — a positional `Vec<String>`
    /// built from a caller-supplied column list, which is what a *dynamic*
    /// caller such as the Python binding has — gets no header at all (serde
    /// introspection reports no named fields, the same as for a tuple). This is
    /// the escape hatch for that case: pass the columns and they are written as
    /// the header, after the leading `time` column.
    ///
    /// `header` is the record's own columns; the `time` column is prepended for
    /// you, exactly as `csv_write` does. An empty `header` writes no header row
    /// at all, matching the positional-tuple behaviour.
    ///
    /// # Errors
    ///
    /// As [`csv_write`](Self::csv_write). Additionally, the row width is *not*
    /// checked against `header` — serde decides the row shape, so a mismatched
    /// header is a caller error that shows up as a ragged file rather than a
    /// wiring failure.
    fn csv_write_with_header(
        &self,
        path: impl AsRef<Path>,
        header: &[String],
    ) -> Result<Stream<()>>;
}

impl<T> CsvSinkOps<T> for Stream<Burst<T>>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
{
    fn csv_write(&self, path: impl AsRef<Path>) -> Result<Stream<()>> {
        self.write_rows(path.as_ref(), None)
    }

    fn csv_write_with_header(
        &self,
        path: impl AsRef<Path>,
        header: &[String],
    ) -> Result<Stream<()>> {
        self.write_rows(path.as_ref(), Some(header))
    }
}

impl<T> Stream<Burst<T>>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
{
    /// The shared body of both sink methods. `header` is `None` to derive the
    /// column names from `T` (the `csv_write` default) or `Some(cols)` to use
    /// the caller's.
    fn write_rows(&self, path: &Path, header: Option<&[String]>) -> Result<Stream<()>> {
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)
            .with_context(|| format!("csv_write: failed to open {} for writing", path.display()))?;
        match header {
            Some(cols) => write_given_header(&mut writer, cols),
            None => write_header::<T>(&mut writer),
        }
        .with_context(|| format!("csv_write: writing header to {}", path.display()))?;
        let display = path.display().to_string();
        Ok(self
            .with_time()
            .for_each_mut(writer, move |w, (time, burst): &(NanoTime, Burst<T>)| {
                for rec in burst.iter() {
                    w.serialize((*time, rec)).with_context(|| {
                        format!("csv_write: failed to serialize record to {display}")
                    })?;
                }
                w.flush()
                    .with_context(|| format!("csv_write: flushing {display}"))?;
                Ok(())
            }))
    }
}

/// Single-value convenience: a plain `Stream<T>` (not `Stream<Burst<T>>`)
/// writes each value as one `(time, record)` row, wrapping it into a
/// one-element burst first — so callers never wrap by hand (matching the
/// `EtcdSinkOps for Stream<EtcdEntry>` convention).
///
/// This second impl is unambiguous with the `Stream<Burst<T>>` one only because
/// `Burst<T>` (a `tinyvec`) is **not** `Serialize` in this build (the `serde`
/// feature is off), so a `Stream<Burst<U>>` never matches this `Stream<T>` impl
/// — the parsing counterpart of the constraint that blocks the same convenience
/// on [`LinesSinkOps`](crate::adapters::lines::LinesSinkOps), where `Burst<T>`
/// *is* `Display`.
impl<T> CsvSinkOps<T> for Stream<T>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
{
    fn csv_write(&self, path: impl AsRef<Path>) -> Result<Stream<()>> {
        self.map(|v: &T| -> Burst<T> { burst![v.clone()] })
            .csv_write(path)
    }

    fn csv_write_with_header(
        &self,
        path: impl AsRef<Path>,
        header: &[String],
    ) -> Result<Stream<()>> {
        self.map(|v: &T| -> Burst<T> { burst![v.clone()] })
            .csv_write_with_header(path, header)
    }
}

/// Write a caller-supplied CSV header: a leading `time` field followed by
/// `cols`. An empty `cols` writes nothing, matching the tuple-record case in
/// [`write_header`].
fn write_given_header(writer: &mut csv::Writer<File>, cols: &[String]) -> Result<()> {
    if !cols.is_empty() {
        writer
            .write_field("time")
            .context("failed to write CSV time header")?;
        writer
            .write_record(cols)
            .context("failed to write CSV header record")?;
    }
    Ok(())
}

/// Write the CSV header: a leading `time` field followed by the record's serde
/// field names. A positional tuple record has no named fields, so nothing is
/// written — matching the legacy adapter (whose tuple output has no header).
fn write_header<T: Serialize + DeserializeOwned + 'static>(
    writer: &mut csv::Writer<File>,
) -> Result<()> {
    let fields = serde_introspect::<T>();
    if !fields.is_empty() {
        writer
            .write_field("time")
            .context("failed to write CSV time header")?;
        writer
            .write_record(fields)
            .context("failed to write CSV header record")?;
    }
    Ok(())
}
