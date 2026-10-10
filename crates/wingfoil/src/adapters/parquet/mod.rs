//! An Apache Parquet file adapter (`.parquet`) — a serde-typed, lazy,
//! bounded-memory replay **source** and a row-grouped, compressed file **sink**,
//! with optional Hive-style time partitioning. The archive / backtest cousin of
//! the [`arrow`](crate::adapters::arrow) adapter, built on the same columnar
//! core.
//!
//! Records are ordinary Rust structs that implement [`serde::Serialize`] /
//! [`serde::de::DeserializeOwned`] — the same named structs the
//! [`csv`](crate::adapters::csv) and [`arrow`](crate::adapters::arrow) adapters
//! take. [`serde_arrow`] maps them to and from Arrow record batches and the
//! [`parquet`](::parquet) crate's Arrow reader/writer maps those to and from
//! Parquet, so a record type needs no format-specific code.
//!
//! **This is the compact archive and backtest format.** A Parquet file is
//! write-once: rows are buffered into row groups, compressed, and the footer
//! that makes the file readable is written last. For a capture that must be
//! durable tick by tick and readable while written, use the
//! [`arrow`](crate::adapters::arrow) adapter; compacting a capture into Parquet
//! is then just a graph:
//!
//! ```no_run
//! use serde::{Deserialize, Serialize};
//! use wingfoil::adapters::arrow::arrow_read;
//! use wingfoil::adapters::parquet::{ParquetSinkOps, TimePartition};
//! use wingfoil::prelude::*;
//! use wingfoil::{NanoTime, RunFor, RunMode};
//!
//! /// A row as captured: the arrow sink's `time` column, then the record.
//! #[derive(Debug, Clone, Default, Serialize, Deserialize)]
//! struct Captured {
//!     time: i64,
//!     px: f64,
//! }
//!
//! /// The record as archived; the parquet sink adds its own `time` column.
//! #[derive(Debug, Clone, Default, Serialize, Deserialize)]
//! struct Tick {
//!     px: f64,
//! }
//!
//! # fn main() -> anyhow::Result<()> {
//! let g = GraphBuilder::new();
//! let _sink = arrow_read(&g, "capture", |c: &Captured| NanoTime::new(c.time as u64))?
//!     .map(|b: &Burst<Captured>| b.iter().map(|c| Tick { px: c.px }).collect::<Burst<Tick>>())
//!     .parquet_write_partitioned("archive", TimePartition::Day)?;
//! g.build()
//!     .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)?;
//! # Ok(())
//! # }
//! ```
//!
//! Replayed at the captured times, the archive's `time` column equals the
//! capture's. Drop `time` from the written type, as here: a record field named
//! like the sink's time column is a wiring error.
//!
//! # Deviations
//!
//! **None from legacy — there is no legacy `parquet` adapter.** This module is
//! new capability in wingfoil (like [`lines`](crate::adapters::lines),
//! [`ws`](crate::adapters::ws) and [`arrow`](crate::adapters::arrow)), so
//! nothing here can regress against a parity oracle and the deviation register
//! carries no row for it. Departures from the `/new-adapter` conventions are
//! deliberate:
//!
//! 1. **The sink is `for_each` + [`finally`](crate::fluent::StreamOps::finally),
//!    not `for_each_mut`** — via the shared columnar core, exactly as for
//!    [`arrow`](crate::adapters::arrow). The writer must be *closed* at
//!    teardown (pending rows written, the open row group flushed, the footer
//!    appended), and `for_each_mut` has no end-of-run hook. `finally` runs even
//!    when a cycle aborts the run, so the file is still finished.
//! 2. **Rows are buffered between ticks.** There is no per-tick flush: rows
//!    accumulate to [`batch_size`](ParquetWriteOptions::batch_size) before they
//!    become a record batch, and batches accumulate to
//!    [`row_group_size`](ParquetWriteOptions::row_group_size) before a row
//!    group is encoded and written. Parquet's whole value — columnar
//!    compression and statistics per row group — depends on groups being
//!    large, and the footer is only written at the end anyway, so flushing per
//!    tick would buy no readability and cost the format its point.
//! 3. **Arrow IPC is the live-capture format; compaction is a graph.** There is
//!    no "convert" API here or in [`arrow`](crate::adapters::arrow): read the
//!    capture and write it out, as above.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / [`csv`](crate::adapters::csv)
//! pattern, the adapter is *not* in the [`prelude`](crate::prelude). Bring in
//! what you need explicitly:
//!
//! - **Source** — the free builder function [`parquet_read`] on a
//!   [`GraphBuilder`] (and [`parquet_read_with_options`] for
//!   [`ParquetReadOptions`]): deterministic historical replay of a `.parquet`
//!   file or a directory tree of them, emitting `Stream<Burst<T>>`.
//! - **Sink** — the [`ParquetSinkOps`] extension trait on `Stream<Burst<T>>`
//!   (and, for convenience, `Stream<T>` — each value auto-wrapped into a
//!   one-element burst), enabled with
//!   `use wingfoil::adapters::parquet::ParquetSinkOps;`.
//!   [`parquet_write`](ParquetSinkOps::parquet_write) writes one file;
//!   [`parquet_write_partitioned`](ParquetSinkOps::parquet_write_partitioned)
//!   writes a Hive-partitioned tree (granularity [`TimePartition`]).
//!
//! # Historical replay (the burst model)
//!
//! [`parquet_read`] opens the file and reads its **footer** at wiring
//! (fail-fast: a missing path, a file that is not Parquet, or a directory with
//! no `.parquet` file is an `Err` before the run), then streams its rows
//! **lazily** over a [`produce_async`](crate::async_source::produce_async)
//! producer: row groups are fetched as the reader reaches them, decoded
//! [`batch_size`](ParquetReadOptions::batch_size) rows at a time, deserialized
//! into `T`s and drained before the next batch is touched, each row stamped
//! with `get_time(&record)`. The channel receiver groups rows sharing a
//! timestamp into one atomic [`Burst`] and replays them deterministically on
//! the graph clock — lossless, in order, independent of wall-clock, and
//! independent of how batches and row groups fall against timestamp groups.
//! Timestamps must be **non-decreasing** across the whole replay (an
//! out-of-order row aborts the run); run with `RunMode::HistoricalFrom(t)` at
//! or before the first row. Under `RunMode::RealTime` rows are delivered as
//! they decode, not paced to their timestamps.
//!
//! **Memory is bounded by a row group, not the file.** The reader holds one row
//! group's column chunks plus one decoded batch (and its `Vec<T>`) per open
//! file, and [`ParquetReadOptions::buffer_size`] bounds how many
//! timestamp-groups the producer may run ahead of the graph. The default is
//! bounded; `None` opts in to an unbounded look-ahead.
//!
//! Columns the file has that `T` does not name are ignored, so a file written
//! by [`parquet_write`](ParquetSinkOps::parquet_write) reads back into the
//! record type that produced it without its `time` column appearing in `T`. To
//! replay at the graph times that wrote it, name the column in `T` as an `i64`
//! (nanoseconds since the epoch — the `Timestamp` column does not deserialize
//! into `u64` or [`NanoTime`]) and stamp with `|r| NanoTime::new(r.time as u64)`.
//! Only the snappy and zstd codecs are compiled in; a file compressed with
//! another codec (gzip, brotli, lz4) fails to decode, mid-stream.
//!
//! # Hive partitioning
//!
//! [`parquet_write_partitioned`](ParquetSinkOps::parquet_write_partitioned)
//! takes a **root directory** and a [`TimePartition`], and writes each row to
//! the file for its graph time's partition, laid out the way Hive, Spark,
//! DuckDB, Polars, pyarrow and pandas discover natively — nested `key=value`
//! directories in **UTC**, zero-padded so lexical order is time order:
//!
//! ```text
//! root/year=2026/month=10/day=04/hour=09/data.parquet   # Hour
//! root/year=2026/month=10/day=04/data.parquet           # Day
//! root/year=2026/month=10/data.parquet                  # Month
//! root/year=2026/data.parquet                           # Year
//! ```
//!
//! Graph time is monotonic in both run modes, so partitions arrive in order and
//! the sink keeps **one file open**: when a row's partition differs from the
//! open file's, that file is finished — footer and all — and the next one
//! created (its directory on demand). So while a partitioned run is live, every
//! partition **before** the current one is a complete, readable file. A burst
//! shares one graph time, so it never straddles two partitions, and a partition
//! with no rows gets no directory. Unless
//! [`time_column`](ParquetWriteOptions::time_column) is `None`, each file still
//! carries the `time` column, so it is self-describing without its path.
//!
//! The file inside each partition is [`ParquetWriteOptions::file_name`]
//! (`data.parquet` by default) and is **truncated** if it exists — two runs
//! into one root overwrite each other's partitions unless they use different
//! file names.
//!
//! Given a **directory**, [`parquet_read`] walks it recursively for
//! `*.parquet` files (ignoring everything else — `_SUCCESS` markers, sidecars)
//! and replays them as a single stream, so a tree written at any granularity
//! reads back as the stream that wrote it. Directories are replayed in path
//! order, one after another; the files within one directory — one partition,
//! possibly written by several runs under different file names — are **merged
//! by time**, ties going to the file that sorts first. The first directory's
//! files are opened (footers read) at wiring; the rest as the replay reaches
//! them, one directory at a time.
//!
//! # Sink
//!
//! [`ParquetSinkOps::parquet_write`] traces the Arrow schema from `T`'s serde
//! shape and creates the file at wiring (fail-fast; a type that cannot be
//! traced — not a struct with named fields — or one with a field named like
//! the time column is a wiring error naming it). Each
//! tick pushes the burst's rows into a `serde_arrow` builder one record at a
//! time — a leading `time` column carrying the graph time as
//! `Timestamp(Nanosecond, None)` (so pandas, DuckDB, Polars and Spark read it
//! as a datetime), then `T`'s fields. When
//! [`batch_size`](ParquetWriteOptions::batch_size) rows are pending they become
//! one record batch handed to the Parquet writer, which encodes a row group
//! each time [`row_group_size`](ParquetWriteOptions::row_group_size) rows have
//! accumulated. Teardown writes what is still pending, flushes the last
//! (short) row group and writes the footer — after a clean run *or* an aborted
//! one.
//!
//! The consequence of buffering: **a single Parquet file is not readable
//! mid-run.** Its footer does not exist until teardown, so a crash (as opposed
//! to an aborted run, which still reaches teardown) leaves a file no reader
//! will open. In a partitioned tree only the open partition is at risk. If
//! that matters, capture with [`arrow`](crate::adapters::arrow) and compact.
//!
//! The sink is run-mode agnostic: the `time` column is the replayed engine
//! time in a backtest and the live engine time in real time. Set
//! [`ParquetWriteOptions::time_column`] to `None` to write `T`'s columns only.
//! Every I/O, encoding or serialization error aborts the run with
//! `parquet_write` and path context.

use std::fs::File;
use std::path::Path;

use ::parquet::arrow::ArrowWriter;
use ::parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};
use ::parquet::basic::{Compression, ZstdLevel};
use ::parquet::file::properties::WriterProperties;
use anyhow::{Context, Result};
use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;
use serde::Serialize;
use serde::de::DeserializeOwned;
use wingfoil::NanoTime;

use crate::adapters::arrow::columnar::{
    self, BatchFileWriter, SinkConfig, SinkTarget, replay_batch_files,
};
use crate::fluent::{GraphBuilder, Stream, StreamOps};
use crate::{Burst, burst};

pub use crate::adapters::arrow::TimePartition;

/// The file extension [`parquet_read`] looks for in a directory, and the one
/// the partitioned sink's default [`file_name`](ParquetWriteOptions::file_name)
/// carries.
const EXTENSION: &str = "parquet";

/// Knobs for [`parquet_read_with_options`]; [`parquet_read`] uses the default.
///
/// The default is **bounded** — that is the point of a streaming reader — and
/// is pinned by a unit test, because a default is a compatibility surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParquetReadOptions {
    /// Rows per decoded record batch — the unit the reader deserializes into
    /// `T` at a time. Default `1024`, the [`parquet`](::parquet) crate's own.
    /// Batches need not align with row groups or timestamp groups: bursts are
    /// regrouped by time downstream. A value of `0` is treated as `1`.
    pub batch_size: usize,
    /// How far the producer may run ahead of the graph, in timestamp-groups
    /// (see [`produce_async`](crate::async_source::produce_async)); `None` is
    /// unbounded. Default `Some(1024)`.
    pub buffer_size: Option<usize>,
}

impl Default for ParquetReadOptions {
    fn default() -> Self {
        Self {
            batch_size: 1024,
            buffer_size: Some(1024),
        }
    }
}

/// The compression codec applied to every column chunk the sink writes.
///
/// Only the codecs compiled into this build are offered; the enum is
/// `#[non_exhaustive]` so more can be added without a breaking change.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ParquetCompression {
    /// No compression.
    Uncompressed,
    /// Snappy — fast, modest ratio, and the de-facto default every Parquet
    /// reader supports. The default.
    #[default]
    Snappy,
    /// Zstandard at its default level — a markedly better ratio for a little
    /// more CPU; the better choice for cold archives.
    Zstd,
}

impl ParquetCompression {
    fn codec(self) -> Compression {
        match self {
            Self::Uncompressed => Compression::UNCOMPRESSED,
            Self::Snappy => Compression::SNAPPY,
            Self::Zstd => Compression::ZSTD(ZstdLevel::default()),
        }
    }
}

/// Knobs for [`ParquetSinkOps::parquet_write_with_options`] and
/// [`ParquetSinkOps::parquet_write_partitioned_with_options`]; the plain
/// methods use the default, which is pinned by a unit test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParquetWriteOptions {
    /// Most rows in one record batch handed to the Parquet writer: once this
    /// many are pending they are written, mid-tick if need be. Default `1024`.
    /// A value of `0` is treated as `1`.
    pub batch_size: usize,
    /// Most rows in one Parquet row group: the writer encodes, compresses and
    /// writes a row group each time this many rows have accumulated, and a
    /// final short one at teardown. Default `65_536` — between the
    /// [`parquet`](::parquet) crate's 1 Mi-row default, tuned for analytic
    /// scans over huge tables, and small groups, which multiply per-group
    /// metadata and make files slow to read. It also bounds what the writer
    /// holds in memory and what a reader must load to decode a row. A value of
    /// `0` is treated as `1`.
    pub row_group_size: usize,
    /// The codec for every column chunk. Default [`ParquetCompression::Snappy`].
    pub compression: ParquetCompression,
    /// Name of the leading graph-time column, a nanosecond `Timestamp` with no
    /// time zone; `None` writes `T`'s columns only. Default `Some("time")`,
    /// matching the csv and arrow sinks.
    pub time_column: Option<String>,
    /// **Partitioned writes only.** The file created inside each partition
    /// directory. Default `data.parquet`. Keep the `.parquet` extension if the
    /// tree is to be read back with [`parquet_read`].
    pub file_name: String,
}

impl Default for ParquetWriteOptions {
    fn default() -> Self {
        Self {
            batch_size: 1024,
            row_group_size: 65_536,
            compression: ParquetCompression::default(),
            time_column: Some("time".to_owned()),
            file_name: format!("data.{EXTENSION}"),
        }
    }
}

/// Deterministic historical replay of a Parquet file — or a directory tree of
/// them — with the default [`ParquetReadOptions`]. See
/// [`parquet_read_with_options`].
///
/// # Errors
///
/// As [`parquet_read_with_options`].
pub fn parquet_read<T, F>(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    get_time: F,
) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + DeserializeOwned + Send + 'static,
    F: Fn(&T) -> NanoTime + Send + 'static,
{
    parquet_read_with_options(g, path, get_time, ParquetReadOptions::default())
}

/// Deterministic historical replay of a Parquet file — or, given a directory,
/// of every `*.parquet` file under it in path order (a Hive-partitioned tree
/// reads back in time order): each record is emitted as a [`Burst<T>`] on the
/// graph clock at `get_time(&record)`, with records sharing a timestamp
/// grouped into one atomic burst.
///
/// The (first) file is opened and its footer read at wiring (fail-fast); row
/// groups are then fetched and decoded **lazily**, a batch at a time, over a
/// [`produce_async`](crate::async_source::produce_async) producer paced by
/// [`buffer_size`](ParquetReadOptions::buffer_size) — the file is never read
/// into memory up front. Run the graph with `RunMode::HistoricalFrom(t)` where
/// `t` is at or before the first record's timestamp. Columns `T` does not name
/// are ignored. See the module docs for the memory story.
///
/// # Errors
///
/// Returns an error at **wiring time** if the path cannot be opened
/// (`"parquet_read: failed to open …"`), is not a Parquet file
/// (`"parquet_read: failed to read the Parquet footer of …"`), or is a
/// directory with no `*.parquet` file under it (`"parquet_read: no .parquet
/// files under …"`). A later file that cannot be opened, or a batch that fails
/// to decode or deserialize into `T`, does not panic — it surfaces as a run
/// failure **mid-stream**, with `parquet_read` and file context, as the reader
/// reaches it. Record timestamps must be non-decreasing across the whole
/// replay; an out-of-order record fails the run.
pub fn parquet_read_with_options<T, F>(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    get_time: F,
    options: ParquetReadOptions,
) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + DeserializeOwned + Send + 'static,
    F: Fn(&T) -> NanoTime + Send + 'static,
{
    let files = columnar::input_files(path.as_ref(), EXTENSION, "parquet_read")?;
    let batch_size = options.batch_size.max(1);
    replay_batch_files(
        g,
        "parquet_read",
        files,
        move |path: &Path| open_file(path, batch_size),
        get_time,
        options.buffer_size,
    )
}

/// Open one file and read its footer. Reads no row group.
fn open_file(path: &Path, batch_size: usize) -> Result<ParquetRecordBatchReader> {
    let display = path.display();
    let file =
        File::open(path).with_context(|| format!("parquet_read: failed to open {display}"))?;
    let context = || format!("parquet_read: failed to read the Parquet footer of {display}");
    ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(context)?
        .with_batch_size(batch_size)
        .build()
        .with_context(context)
}

/// A Parquet file sink — the outbound counterpart of [`parquet_read`].
///
/// An extension trait on `Stream<Burst<T>>` — and, for convenience,
/// `Stream<T>` (each value auto-wrapped into a one-element burst) — so `use`ing
/// it enables `stream.parquet_write(path)` chaining. Each emitted burst
/// appends every record as one `(time, record)` row; rows are buffered into
/// record batches and row groups (see [`ParquetWriteOptions`]), and the file's
/// footer is written at teardown. An I/O, encoding or serialization error
/// aborts the run with context.
///
/// The two plain methods are provided; an implementor writes the two
/// `*_with_options` forms.
pub trait ParquetSinkOps<T> {
    /// Write each record to `path` as a `(time, record)` row with the default
    /// [`ParquetWriteOptions`], **truncating** the file first (created if
    /// absent). Returns the sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// As [`parquet_write_with_options`](Self::parquet_write_with_options).
    fn parquet_write(&self, path: impl AsRef<Path>) -> Result<Stream<()>> {
        self.parquet_write_with_options(path, ParquetWriteOptions::default())
    }

    /// Like [`parquet_write`](Self::parquet_write), with explicit options.
    /// [`file_name`](ParquetWriteOptions::file_name) is ignored: `path` is the
    /// file.
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time if `T`'s Arrow schema cannot be traced
    /// from its serde shape, if `T` has a field named like
    /// [`time_column`](ParquetWriteOptions::time_column), or if the file
    /// cannot be created or the Parquet
    /// writer cannot be built for that schema. A per-row serialization,
    /// encoding or I/O failure, or a failure to finish the file at teardown,
    /// aborts the run with context.
    fn parquet_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: ParquetWriteOptions,
    ) -> Result<Stream<()>>;

    /// Write each record under `root` in a Hive-partitioned layout at the given
    /// granularity (see the module docs), with the default
    /// [`ParquetWriteOptions`]. Returns the sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// As [`parquet_write_partitioned_with_options`](Self::parquet_write_partitioned_with_options).
    fn parquet_write_partitioned(
        &self,
        root: impl AsRef<Path>,
        partition: TimePartition,
    ) -> Result<Stream<()>> {
        self.parquet_write_partitioned_with_options(root, partition, ParquetWriteOptions::default())
    }

    /// Like [`parquet_write_partitioned`](Self::parquet_write_partitioned),
    /// with explicit options — including the per-partition
    /// [`file_name`](ParquetWriteOptions::file_name).
    ///
    /// # Errors
    ///
    /// Returns an error at wiring time if `T`'s Arrow schema cannot be traced
    /// from its serde shape, if `T` has a field named like
    /// [`time_column`](ParquetWriteOptions::time_column), or if `root` cannot
    /// be created. Each partition's
    /// directory and file are created when its first row arrives, so a failure
    /// there — or a per-row serialization, encoding or I/O failure, or a
    /// failure to finish a file — aborts the run with context.
    fn parquet_write_partitioned_with_options(
        &self,
        root: impl AsRef<Path>,
        partition: TimePartition,
        options: ParquetWriteOptions,
    ) -> Result<Stream<()>>;
}

impl<T> ParquetSinkOps<T> for Stream<Burst<T>>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
{
    fn parquet_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: ParquetWriteOptions,
    ) -> Result<Stream<()>> {
        let properties = writer_properties(&options);
        columnar::wire_batch_sink::<T, ParquetFile>(
            self,
            "parquet_write",
            SinkTarget::File(path.as_ref().to_path_buf()),
            sink_config(options),
            properties,
        )
    }

    fn parquet_write_partitioned_with_options(
        &self,
        root: impl AsRef<Path>,
        partition: TimePartition,
        options: ParquetWriteOptions,
    ) -> Result<Stream<()>> {
        let properties = writer_properties(&options);
        let file_name = options.file_name.clone();
        columnar::wire_batch_sink::<T, ParquetFile>(
            self,
            "parquet_write",
            SinkTarget::Partitioned {
                root: root.as_ref().to_path_buf(),
                partition,
                file_name,
            },
            sink_config(options),
            properties,
        )
    }
}

/// Single-value convenience: a plain `Stream<T>` (not `Stream<Burst<T>>`)
/// writes each value as one `(time, record)` row, wrapping it into a
/// one-element burst first — so callers never wrap by hand.
///
/// Unambiguous with the `Stream<Burst<T>>` impl for the same reason the csv
/// and arrow pairs are: `Burst<T>` is not `Serialize` in this build, so a
/// `Stream<Burst<U>>` never matches this impl.
impl<T> ParquetSinkOps<T> for Stream<T>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
{
    fn parquet_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: ParquetWriteOptions,
    ) -> Result<Stream<()>> {
        self.map(|v: &T| -> Burst<T> { burst![v.clone()] })
            .parquet_write_with_options(path, options)
    }

    fn parquet_write_partitioned_with_options(
        &self,
        root: impl AsRef<Path>,
        partition: TimePartition,
        options: ParquetWriteOptions,
    ) -> Result<Stream<()>> {
        self.map(|v: &T| -> Burst<T> { burst![v.clone()] })
            .parquet_write_partitioned_with_options(root, partition, options)
    }
}

/// The columnar core's format-independent half of [`ParquetWriteOptions`]
/// (`file_name` travels in the partitioned [`SinkTarget`]). Never flushes per
/// tick: see the module's `# Deviations`.
fn sink_config(options: ParquetWriteOptions) -> SinkConfig {
    SinkConfig {
        batch_size: options.batch_size,
        time_column: options.time_column,
        flush_every_tick: false,
    }
}

/// The Parquet-specific half of [`ParquetWriteOptions`]: codec and row group
/// size, applied to every file the sink creates.
fn writer_properties(options: &ParquetWriteOptions) -> WriterProperties {
    WriterProperties::builder()
        .set_compression(options.compression.codec())
        .set_max_row_group_row_count(Some(options.row_group_size.max(1)))
        .build()
}

/// One `.parquet` file: the [`parquet`](::parquet) crate's Arrow writer over
/// the file. It buffers rows into the open row group and writes each group as
/// it fills; [`finish`](BatchFileWriter::finish) flushes the last one and
/// writes the footer.
struct ParquetFile {
    writer: ArrowWriter<File>,
}

impl BatchFileWriter for ParquetFile {
    type Options = WriterProperties;

    fn create(path: &Path, schema: &SchemaRef, properties: &WriterProperties) -> Result<Self> {
        let file = File::create(path)?;
        let writer = ArrowWriter::try_new(file, schema.clone(), Some(properties.clone()))?;
        Ok(Self { writer })
    }

    fn write(&mut self, batch: &RecordBatch) -> Result<()> {
        self.writer.write(batch)?;
        Ok(())
    }

    fn finish(self) -> Result<()> {
        self.writer.close()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `Default` is a compatibility surface: pinned so a change is a
    /// decision, not a drift.
    #[test]
    fn read_defaults_are_pinned() {
        let d = ParquetReadOptions::default();
        assert_eq!(d.batch_size, 1024);
        assert_eq!(d.buffer_size, Some(1024));
    }

    #[test]
    fn write_defaults_are_pinned() {
        let d = ParquetWriteOptions::default();
        assert_eq!(d.batch_size, 1024);
        assert_eq!(d.row_group_size, 65_536);
        assert_eq!(d.compression, ParquetCompression::Snappy);
        assert_eq!(d.time_column.as_deref(), Some("time"));
        assert_eq!(d.file_name, "data.parquet");
    }

    #[test]
    fn compression_maps_to_the_parquet_codec() {
        assert_eq!(
            ParquetCompression::Uncompressed.codec(),
            Compression::UNCOMPRESSED
        );
        assert_eq!(ParquetCompression::Snappy.codec(), Compression::SNAPPY);
        assert_eq!(
            ParquetCompression::Zstd.codec(),
            Compression::ZSTD(ZstdLevel::default())
        );
    }

    #[test]
    fn writer_properties_carry_codec_and_row_group_size() {
        let options = ParquetWriteOptions {
            compression: ParquetCompression::Zstd,
            row_group_size: 0,
            ..Default::default()
        };
        let props = writer_properties(&options);
        assert_eq!(
            props.compression(&::parquet::schema::types::ColumnPath::from("time")),
            Compression::ZSTD(ZstdLevel::default())
        );
        assert_eq!(props.max_row_group_row_count(), Some(1));
    }
}
