//! An Apache Arrow IPC **streaming-format** file adapter (`.arrows`) — a
//! serde-typed, lazy, bounded-memory replay **source** and an append-only,
//! flush-per-tick, crash-tolerant file **sink**, with optional Hive-style time
//! partitioning. The columnar cousin of the [`csv`](crate::adapters::csv)
//! adapter.
//!
//! Records are ordinary Rust structs that implement [`serde::Serialize`] /
//! [`serde::de::DeserializeOwned`] — the same named structs the csv adapter
//! takes. [`serde_arrow`] maps them to and from Arrow record batches, so a
//! record type needs no Arrow-specific code.
//!
//! **This is the durable live-capture format.** Every tick's rows reach the OS
//! as one self-contained IPC message, so a file is readable while it is being
//! written and a crash loses at most the rows of the tick in flight. The
//! `parquet` adapter is the compact archive / backtest format, built on the
//! same columnar core (`adapters/arrow/columnar.rs`); compacting a capture is
//! just a graph — `arrow_read(dir)` into `parquet_write_partitioned(...)`.
//!
//! # Deviations
//!
//! **None from legacy — there is no legacy `arrow` adapter.** This module is new
//! capability in wingfoil (like [`lines`](crate::adapters::lines) and
//! [`ws`](crate::adapters::ws)), so nothing here can regress against a parity
//! oracle and the deviation register carries no row for it. Two departures from
//! the `/new-adapter` conventions are deliberate:
//!
//! 1. **The sink is `for_each` + [`finally`](crate::fluent::StreamOps::finally),
//!    not `for_each_mut`.** The writer must be *finished* at teardown — the
//!    pending rows written and the end-of-stream marker appended — and
//!    `for_each_mut` has no end-of-run hook. The writer lives in an
//!    `Rc<RefCell<_>>` shared by the per-tick closure and the `finally`
//!    closure; `finally` runs even when a cycle aborts the run, so the file
//!    holds every row pushed before the abort.
//! 2. **Per-tick flush is the default** ([`ArrowWriteOptions::flush_every_tick`]).
//!    A columnar format usually batches for size; this one's job is durable live
//!    capture, so each tick becomes its own record batch and is flushed to the
//!    OS before the cycle ends. Set it to `false` (with a larger
//!    [`batch_size`](ArrowWriteOptions::batch_size)) for fewer, larger batches
//!    when durability per tick does not matter.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`csv`](crate::adapters::csv)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — the free builder function [`arrow_read`] on a
//!   [`GraphBuilder`] (and [`arrow_read_with_options`] for
//!   [`ArrowReadOptions`]): deterministic historical replay of an `.arrows`
//!   file or a directory tree of them, emitting `Stream<Burst<T>>`.
//! - **Sink** — the [`ArrowSinkOps`] extension trait on `Stream<Burst<T>>` (and,
//!   for convenience, `Stream<T>` — each value auto-wrapped into a one-element
//!   burst), enabled with `use wingfoil::adapters::arrow::ArrowSinkOps;`.
//!   [`arrow_write`](ArrowSinkOps::arrow_write) writes one file;
//!   [`arrow_write_partitioned`](ArrowSinkOps::arrow_write_partitioned) writes a
//!   Hive-partitioned tree (granularity [`TimePartition`]).
//!
//! # Historical replay (the burst model)
//!
//! [`arrow_read`] opens the file and reads its schema message at wiring
//! (fail-fast: a missing path, a file that is not an Arrow IPC stream, or a
//! directory with no `.arrows` file is an `Err` before the run), then streams
//! its rows **lazily** over a
//! [`produce_async`](crate::async_source::produce_async) producer: one record
//! batch is read, deserialized into `T`s and drained before the next is
//! touched, each row stamped with `get_time(&record)`. The channel receiver
//! groups rows sharing a timestamp into one atomic [`Burst`] and
//! replays them deterministically on the graph clock — lossless, in order,
//! independent of wall-clock. Timestamps must be **non-decreasing** across the
//! whole replay (an out-of-order row aborts the run); run with
//! `RunMode::HistoricalFrom(t)` at or before the first row. Under
//! `RunMode::RealTime` rows are delivered as they decode, not paced to their
//! timestamps.
//!
//! **Memory is bounded by the file's batches, not its size.** The reader holds
//! one decoded record batch (and its `Vec<T>`) at a time, and
//! [`ArrowReadOptions::buffer_size`] bounds how many timestamp-groups the
//! producer may run ahead of the graph. The default is bounded; `None` opts in
//! to an unbounded look-ahead. Batches are whatever the writer wrote — one per
//! tick from this adapter's sink by default, up to
//! [`batch_size`](ArrowWriteOptions::batch_size) rows — so there is no
//! reader-side batch size.
//!
//! Columns the file has that `T` does not name are ignored, so a file written by
//! [`arrow_write`](ArrowSinkOps::arrow_write) reads back into the record type
//! that produced it without its `time` column appearing in `T`. To replay a
//! capture at the graph times that wrote it, name the column in `T` as an
//! `i64` (nanoseconds since the epoch — the `Timestamp` column does not
//! deserialize into `u64` or [`NanoTime`]) and stamp with
//! `|r| NanoTime::new(r.time as u64)`. Compressed IPC
//! streams (lz4 / zstd buffers) are not supported: the codecs are not compiled
//! in, and such a batch fails to decode.
//!
//! # Hive partitioning
//!
//! [`arrow_write_partitioned`](ArrowSinkOps::arrow_write_partitioned) takes a
//! **root directory** and a [`TimePartition`], and writes each row to the file
//! for its graph time's partition, laid out the way Hive, Spark, DuckDB,
//! pyarrow and pandas discover natively — nested `key=value` directories in
//! **UTC**, zero-padded so lexical order is time order:
//!
//! ```text
//! root/year=2026/month=10/day=04/hour=09/data.arrows     # TimePartition::Hour
//! root/year=2026/month=10/day=04/data.arrows             # TimePartition::Day
//! root/year=2026/month=10/data.arrows                    # TimePartition::Month
//! root/year=2026/data.arrows                             # TimePartition::Year
//! ```
//!
//! Graph time is monotonic in both run modes, so partitions arrive in order and
//! the sink keeps **one file open**: when a row's partition differs from the
//! open file's, that file is finished and the next one created (its directory
//! on demand). A burst shares one graph time, so it never straddles two
//! partitions, and a partition with no rows gets no directory. Each file still
//! carries the `time` column, so it is self-describing without its path.
//!
//! The file inside each partition is [`ArrowWriteOptions::file_name`]
//! (`data.arrows` by default) and is **truncated** if it exists — two runs into
//! one root overwrite each other's partitions unless they use different file
//! names.
//!
//! Given a **directory**, [`arrow_read`] walks it recursively for `*.arrows`
//! files (ignoring everything else) and replays them as a single stream, so a
//! tree written at any granularity reads back as the stream that wrote it.
//! Directories are replayed in path order, one after another; the files within
//! one directory — one partition, possibly written by several runs under
//! different file names — are **merged by time**, ties going to the file that
//! sorts first. The first directory's files are opened at wiring; the rest as
//! the replay reaches them, one directory at a time.
//!
//! # Sink
//!
//! [`ArrowSinkOps::arrow_write`] traces the Arrow schema from `T`'s serde shape,
//! creates the file and writes the IPC schema message at wiring (fail-fast; a
//! type that cannot be traced — not a struct with named fields — is a wiring
//! error naming it). Each tick pushes the burst's rows into a `serde_arrow`
//! builder one record at a time — a leading `time` column carrying the graph
//! time as `Timestamp(Nanosecond, None)`, then `T`'s fields. When
//! [`batch_size`](ArrowWriteOptions::batch_size) rows are pending, or at the
//! end of the tick when [`flush_every_tick`](ArrowWriteOptions::flush_every_tick),
//! the pending rows become one record batch, appended and flushed to the OS
//! (not `fsync`ed — a power cut is the OS's to survive). Teardown writes what
//! is still pending plus the end-of-stream marker. A file cut off without that
//! marker (a crash) still reads back to its last complete batch.
//!
//! The sink is run-mode agnostic: the `time` column is the replayed engine time
//! in a backtest and the live engine time in real time. Set
//! [`ArrowWriteOptions::time_column`] to `None` to write `T`'s columns only.
//! Every I/O or serialization error aborts the run with `arrow_write` and path
//! context.

pub(crate) mod columnar;

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;

use anyhow::{Context, Result};
use arrow_array::RecordBatch;
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::SchemaRef;
use serde::Serialize;
use serde::de::DeserializeOwned;
use wingfoil::NanoTime;

use crate::fluent::{GraphBuilder, Stream, StreamOps};
use crate::{Burst, burst};

pub use columnar::TimePartition;
use columnar::{BatchFileWriter, SinkConfig, SinkTarget};

/// The file extension [`arrow_read`] looks for in a directory, and the one the
/// partitioned sink's default [`file_name`](ArrowWriteOptions::file_name)
/// carries — the conventional suffix for the Arrow IPC *streaming* format.
const EXTENSION: &str = "arrows";

/// Knobs for [`arrow_read_with_options`]; [`arrow_read`] uses the default.
///
/// The default is **bounded** — that is the point of a streaming reader — and
/// is pinned by a unit test, because a default is a compatibility surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrowReadOptions {
    /// How far the producer may run ahead of the graph, in timestamp-groups
    /// (see [`produce_async`](crate::async_source::produce_async)); `None` is
    /// unbounded. Default `Some(1024)`.
    pub buffer_size: Option<usize>,
}

impl Default for ArrowReadOptions {
    fn default() -> Self {
        Self {
            buffer_size: Some(1024),
        }
    }
}

/// Knobs for [`ArrowSinkOps::arrow_write_with_options`] and
/// [`ArrowSinkOps::arrow_write_partitioned_with_options`]; the plain methods use
/// the default, which is pinned by a unit test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArrowWriteOptions {
    /// Most rows in one record batch: once this many are pending they are
    /// written, mid-tick if need be. Default `1024`. A value of `0` is treated
    /// as `1`.
    pub batch_size: usize,
    /// Write the pending rows as a batch at the end of every tick, so each
    /// tick is on disk before the next cycle. Default `true` — the format's
    /// job is durable live capture. With `false`, rows are written only when
    /// `batch_size` fills, when a partitioned sink moves to the next
    /// partition, and at teardown.
    pub flush_every_tick: bool,
    /// Name of the leading graph-time column, a nanosecond `Timestamp` with no
    /// time zone; `None` writes `T`'s columns only. Default `Some("time")`,
    /// matching the csv sink's leading `time` column.
    pub time_column: Option<String>,
    /// **Partitioned writes only.** The file created inside each partition
    /// directory. Default `data.arrows`. Keep the `.arrows` extension if the
    /// tree is to be read back with [`arrow_read`].
    pub file_name: String,
}

impl Default for ArrowWriteOptions {
    fn default() -> Self {
        Self {
            batch_size: 1024,
            flush_every_tick: true,
            time_column: Some("time".to_owned()),
            file_name: format!("data.{EXTENSION}"),
        }
    }
}

/// Deterministic historical replay of an Arrow IPC stream file — or a directory
/// tree of them — with the default [`ArrowReadOptions`]. See
/// [`arrow_read_with_options`].
///
/// # Errors
///
/// As [`arrow_read_with_options`].
pub fn arrow_read<T, F>(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    get_time: F,
) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + DeserializeOwned + Send + 'static,
    F: Fn(&T) -> NanoTime + Send + 'static,
{
    arrow_read_with_options(g, path, get_time, ArrowReadOptions::default())
}

/// Deterministic historical replay of an Arrow IPC stream file — or, given a
/// directory, of every `*.arrows` file under it in path order (a
/// Hive-partitioned tree reads back in time order): each record is emitted as
/// a [`Burst<T>`] on the graph clock at `get_time(&record)`, with records
/// sharing a timestamp grouped into one atomic burst.
///
/// The (first) file is opened and its schema message read at wiring
/// (fail-fast); record batches are then read **lazily**, one at a time, over a
/// [`produce_async`](crate::async_source::produce_async) producer paced by
/// [`buffer_size`](ArrowReadOptions::buffer_size) — the file is never read
/// into memory up front. Run the graph with `RunMode::HistoricalFrom(t)` where
/// `t` is at or before the first record's timestamp. Columns `T` does not name
/// are ignored. See the module docs for the memory story.
///
/// # Errors
///
/// Returns an error at **wiring time** if the path cannot be opened
/// (`"arrow_read: failed to open …"`), is not an Arrow IPC stream
/// (`"arrow_read: failed to read the Arrow IPC stream header of …"`), or is a
/// directory with no `*.arrows` file under it (`"arrow_read: no .arrows files
/// under …"`). A later file that cannot be opened, or a batch that fails to
/// decode or deserialize into `T`, does not panic — it surfaces as a run
/// failure **mid-stream**, with `arrow_read` and file context, as the reader
/// reaches it. Record timestamps must be non-decreasing across the whole
/// replay; an out-of-order record fails the run.
pub fn arrow_read_with_options<T, F>(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    get_time: F,
    options: ArrowReadOptions,
) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + DeserializeOwned + Send + 'static,
    F: Fn(&T) -> NanoTime + Send + 'static,
{
    let files = columnar::input_files(path.as_ref(), EXTENSION, "arrow_read")?;
    columnar::replay_batch_files(
        g,
        "arrow_read",
        files,
        open_stream,
        get_time,
        options.buffer_size,
    )
}

/// Open one file and read its schema message. Reads no record batch.
fn open_stream(path: &Path) -> Result<StreamReader<BufReader<File>>> {
    let display = path.display();
    let file = File::open(path).with_context(|| format!("arrow_read: failed to open {display}"))?;
    StreamReader::try_new_buffered(file, None).with_context(|| {
        format!("arrow_read: failed to read the Arrow IPC stream header of {display}")
    })
}

/// An Arrow IPC stream file sink — the outbound counterpart of [`arrow_read`].
///
/// An extension trait on `Stream<Burst<T>>` — and, for convenience, `Stream<T>`
/// (each value auto-wrapped into a one-element burst) — so `use`ing it enables
/// `stream.arrow_write(path)` chaining. Each emitted burst appends every record
/// as one `(time, record)` row; rows are written as record batches and flushed
/// per tick by default (see [`ArrowWriteOptions`]), and the file is finished at
/// teardown. An I/O or serialization error aborts the run with context.
///
/// The two plain methods are provided; an implementor writes the two
/// `*_with_options` forms.
pub trait ArrowSinkOps<T> {
    /// Write each record to `path` as a `(time, record)` row with the default
    /// [`ArrowWriteOptions`], **truncating** the file first (created if
    /// absent). Returns the sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// As [`arrow_write_with_options`](Self::arrow_write_with_options).
    fn arrow_write(&self, path: impl AsRef<Path>) -> Result<Stream<()>> {
        self.arrow_write_with_options(path, ArrowWriteOptions::default())
    }

    /// Like [`arrow_write`](Self::arrow_write), with explicit options.
    /// [`file_name`](ArrowWriteOptions::file_name) is ignored: `path` is the
    /// file.
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time if `T`'s Arrow schema cannot be traced
    /// from its serde shape, if `T` has a field named like
    /// [`time_column`](ArrowWriteOptions::time_column), or if the file cannot
    /// be created or its schema
    /// message written. A per-row serialization or I/O failure, or a failure
    /// to finish the file at teardown, aborts the run with context.
    fn arrow_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: ArrowWriteOptions,
    ) -> Result<Stream<()>>;

    /// Write each record under `root` in a Hive-partitioned layout at the given
    /// granularity (see the module docs), with the default
    /// [`ArrowWriteOptions`]. Returns the sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// As [`arrow_write_partitioned_with_options`](Self::arrow_write_partitioned_with_options).
    fn arrow_write_partitioned(
        &self,
        root: impl AsRef<Path>,
        partition: TimePartition,
    ) -> Result<Stream<()>> {
        self.arrow_write_partitioned_with_options(root, partition, ArrowWriteOptions::default())
    }

    /// Like [`arrow_write_partitioned`](Self::arrow_write_partitioned), with
    /// explicit options — including the per-partition
    /// [`file_name`](ArrowWriteOptions::file_name).
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time if `T`'s Arrow schema cannot be traced
    /// from its serde shape, if `T` has a field named like
    /// [`time_column`](ArrowWriteOptions::time_column), or if `root` cannot
    /// be created. Each partition's
    /// directory and file are created when its first row arrives, so a failure
    /// there — or a per-row serialization or I/O failure, or a failure to
    /// finish a file — aborts the run with context.
    fn arrow_write_partitioned_with_options(
        &self,
        root: impl AsRef<Path>,
        partition: TimePartition,
        options: ArrowWriteOptions,
    ) -> Result<Stream<()>>;
}

impl<T> ArrowSinkOps<T> for Stream<Burst<T>>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
{
    fn arrow_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: ArrowWriteOptions,
    ) -> Result<Stream<()>> {
        columnar::wire_batch_sink::<T, IpcFile>(
            self,
            "arrow_write",
            SinkTarget::File(path.as_ref().to_path_buf()),
            sink_config(options),
            (),
        )
    }

    fn arrow_write_partitioned_with_options(
        &self,
        root: impl AsRef<Path>,
        partition: TimePartition,
        options: ArrowWriteOptions,
    ) -> Result<Stream<()>> {
        let file_name = options.file_name.clone();
        columnar::wire_batch_sink::<T, IpcFile>(
            self,
            "arrow_write",
            SinkTarget::Partitioned {
                root: root.as_ref().to_path_buf(),
                partition,
                file_name,
            },
            sink_config(options),
            (),
        )
    }
}

/// Single-value convenience: a plain `Stream<T>` (not `Stream<Burst<T>>`)
/// writes each value as one `(time, record)` row, wrapping it into a
/// one-element burst first — so callers never wrap by hand.
///
/// Unambiguous with the `Stream<Burst<T>>` impl for the same reason the csv
/// pair is: `Burst<T>` is not `Serialize` in this build, so a
/// `Stream<Burst<U>>` never matches this impl.
impl<T> ArrowSinkOps<T> for Stream<T>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
{
    fn arrow_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: ArrowWriteOptions,
    ) -> Result<Stream<()>> {
        self.map(|v: &T| -> Burst<T> { burst![v.clone()] })
            .arrow_write_with_options(path, options)
    }

    fn arrow_write_partitioned_with_options(
        &self,
        root: impl AsRef<Path>,
        partition: TimePartition,
        options: ArrowWriteOptions,
    ) -> Result<Stream<()>> {
        self.map(|v: &T| -> Burst<T> { burst![v.clone()] })
            .arrow_write_partitioned_with_options(root, partition, options)
    }
}

/// The columnar core's format-independent half of [`ArrowWriteOptions`]
/// (`file_name` travels in the partitioned [`SinkTarget`]). The IPC writer
/// itself takes no options.
fn sink_config(options: ArrowWriteOptions) -> SinkConfig {
    SinkConfig {
        batch_size: options.batch_size,
        time_column: options.time_column,
        flush_every_tick: options.flush_every_tick,
    }
}

/// One `.arrows` file: an IPC stream writer over a buffered file. Every batch
/// is flushed to the OS as soon as it is written, which is what makes the file
/// tailable and crash-tolerant.
struct IpcFile {
    writer: StreamWriter<BufWriter<File>>,
}

impl BatchFileWriter for IpcFile {
    type Options = ();

    fn create(path: &Path, schema: &SchemaRef, _options: &()) -> Result<Self> {
        let file = File::create(path)?;
        let mut writer = StreamWriter::try_new_buffered(file, schema)?;
        // The schema message on disk now: a sink that never sees a row still
        // leaves a valid, empty stream behind.
        writer.flush()?;
        Ok(Self { writer })
    }

    fn write(&mut self, batch: &RecordBatch) -> Result<()> {
        self.writer.write(batch)?;
        self.writer.flush()?;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.writer.finish()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Default` is a compatibility surface: pinned so a change is a
    /// decision, not a drift.
    #[test]
    fn read_default_is_bounded() {
        assert_eq!(ArrowReadOptions::default().buffer_size, Some(1024));
    }

    #[test]
    fn write_defaults_are_pinned() {
        let d = ArrowWriteOptions::default();
        assert_eq!(d.batch_size, 1024);
        assert!(d.flush_every_tick);
        assert_eq!(d.time_column.as_deref(), Some("time"));
        assert_eq!(d.file_name, "data.arrows");
    }
}
