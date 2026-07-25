//! A CSV file adapter — a serde-typed replay **source** and a file **sink**,
//! the parsing cousin of the dependency-free [`lines`](crate::adapters::lines)
//! adapter (see `next/docs/port-plan.md`, Phase 4: "csv — replay source + sink;
//! exercises 0.3 historical bursts"). It ports the classic
//! `wingfoil::adapters::csv` module onto the Op model.
//!
//! Records are ordinary Rust types that implement [`serde::Serialize`] /
//! [`serde::de::DeserializeOwned`] — a named struct, or a positional tuple such
//! as `(NanoTime, u32)`. The `csv` crate handles field mapping exactly as it
//! does for the classic adapter.
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
//!   burst), enabled with `use wingfoil_next::adapters::csv::CsvSinkOps;`.
//!
//! # Historical replay (the burst model)
//!
//! [`csv_read`] reads and deserializes the whole file up front, then stamps
//! each record with `get_time(&record)` and queues it onto a
//! [`channel`](crate::fluent::SourceOps::channel) source through
//! [`ChannelSender::send_at`](crate::channel::ChannelSender::send_at); the
//! sender is [`close`](crate::channel::ChannelSender::close)d immediately. The
//! channel receiver groups records sharing a timestamp into one atomic
//! [`Burst`](crate::Burst) and replays them deterministically at their
//! timestamps — lossless, in order, independent of wall-clock. This mirrors the
//! classic `csv_read`, whose `TryIteratorStream` groups same-timestamp rows
//! into one burst (use `.collapse_accumulate()` when the source is strictly
//! ascending and you want a flat `Vec<T>`).
//!
//! Two consequences of using the channel source (deviations from classic):
//! record timestamps must be **non-decreasing**
//! (the historical receiver rejects out-of-order sends), and a row that fails
//! to deserialize is propagated as a [`Message::Error`](crate::channel::Message)
//! so it still aborts the run — the same observable outcome as classic (the run
//! fails with a "failed to deserialize row" context), just surfaced at the
//! start of the replay rather than mid-stream.
//!
//! # Sink
//!
//! [`CsvSinkOps::csv_write`] opens the target file once at wiring time, writes
//! the header up front (a leading `time` column plus the record's serde field
//! names, via `serde_aux` introspection — a positional tuple record has no
//! named fields, so no header is written, matching classic), and returns a
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

use crate::fluent::{GraphBuilder, Stream, StreamOps};
use crate::{Burst, burst};

/// Deterministic historical replay of a CSV file: each record is emitted as a
/// [`Burst<T>`] on the graph clock at `get_time(&record)`, with records sharing
/// a timestamp grouped into one atomic burst.
///
/// The file is read and deserialized in full up front and queued onto a
/// [`channel`](crate::fluent::SourceOps::channel) source, so replay is
/// reproducible and needs no producer thread. Run the graph with
/// `RunMode::HistoricalFrom(t)` where `t` is at or before the first record's
/// timestamp (e.g. `NanoTime::ZERO`). Use
/// [`collapse_accumulate`](crate::fluent::Stream::collapse_accumulate) when the
/// source is strictly ascending and you want a flat `Vec<T>`.
///
/// `has_headers` toggles whether the first line is a header (skipped) or data,
/// exactly as `csv::ReaderBuilder::has_headers`.
///
/// # Errors
///
/// Returns an error if the file cannot be opened. A row that fails to
/// deserialize does not panic — it is propagated into the graph as a channel
/// error and surfaces as a run failure with a "failed to deserialize row"
/// context (the same outcome as the classic adapter). Record timestamps must be
/// non-decreasing; an out-of-order record fails the run.
pub fn csv_read<T, F>(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    get_time: F,
    has_headers: bool,
) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + DeserializeOwned + 'static,
    F: Fn(&T) -> NanoTime,
{
    let path = path.as_ref();
    let file =
        File::open(path).with_context(|| format!("csv_read: failed to open {}", path.display()))?;
    let display = path.display().to_string();
    let reader = csv::ReaderBuilder::new()
        .has_headers(has_headers)
        .from_reader(file);
    // Deserialize lazily into `Result<(record, time)>` rows and hand them to the
    // shared `replay_results` engine: it queues each row via `send_at`, and — on
    // a malformed row — forwards the error via `send_error` and stops, so the run
    // aborts with the same context string the classic adapter uses (mirroring
    // classic's abort-not-panic on a bad row). `into_deserialize` owns the reader
    // so the iterator is `'static`.
    let rows = reader.into_deserialize::<T>().map(move |record| {
        record
            .map(|rec| {
                let time = get_time(&rec);
                (rec, time)
            })
            .map_err(|e| {
                anyhow::Error::new(e).context(format!(
                    "csv_read: failed to deserialize row from {display}"
                ))
            })
    });
    Ok(g.replay_results(rows))
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
}

impl<T> CsvSinkOps<T> for Stream<Burst<T>>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
{
    fn csv_write(&self, path: impl AsRef<Path>) -> Result<Stream<()>> {
        let path = path.as_ref();
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .from_path(path)
            .with_context(|| format!("csv_write: failed to open {} for writing", path.display()))?;
        write_header::<T>(&mut writer)
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
}

/// Write the CSV header: a leading `time` field followed by the record's serde
/// field names. A positional tuple record has no named fields, so nothing is
/// written — matching the classic adapter (whose tuple output has no header).
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
