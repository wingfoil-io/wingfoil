//! polars adapter — replay a [polars](https://pola.rs) `DataFrame` (in memory,
//! Parquet or Arrow IPC) as a timestamped historical source, and collect a row
//! stream back into a `DataFrame` or a Parquet / IPC file. Wingfoil-only: there
//! is no legacy polars adapter.
//!
//! # Layering
//!
//! Following the [`lines`](crate::adapters::lines) / `csv` pattern, the adapter
//! is *not* in the [`prelude`](crate::prelude). Bring in what you need
//! explicitly:
//!
//! - **Source** — the free builder function [`polars_read`] on a
//!   [`GraphBuilder`]: deterministic historical replay of a frame, emitting
//!   `Stream<Burst<PolarsRow>>`.
//! - **Sink** — the [`PolarsSinkOps`] extension trait on
//!   `Stream<Burst<PolarsRow>>` (and, for convenience, `Stream<PolarsRow>`):
//!   [`polars_collect`](PolarsSinkOps::polars_collect) into an in-memory
//!   [`DataFrame`] read back through a [`PolarsCollector`], or
//!   [`polars_write`](PolarsSinkOps::polars_write) to a Parquet / IPC file.
//!
//! The polars crate the adapter is built against is re-exported as
//! [`polars`], so callers (and the Python binding) name its types
//! at exactly the version the adapter compiled with; the handful that appear in
//! this module's signatures are re-exported directly.
//!
//! # Rows
//!
//! A frame is columnar and a graph is a stream of values, so the unit that
//! crosses the edge is one row: a [`PolarsRow`], the frame's [`Schema`] (shared
//! by every row of one read, so a clone is a pointer copy) plus one
//! [`AnyValue`] per column. Read a field with [`PolarsRow::get`] and the usual
//! `AnyValue` accessors (`extract::<f64>()`, …); build one with
//! [`PolarsRow::new`].
//!
//! # Historical replay (the burst model)
//!
//! [`polars_read`] takes a [`PolarsSource`] — a `DataFrame`, or a path whose
//! extension picks the format (`.parquet`/`.pq`, `.arrow`/`.ipc`/`.feather`) —
//! and names its **time column**. That column becomes the graph clock: row `i`
//! is delivered at its time, rows sharing a time ride one atomic
//! [`Burst`], in frame order. The time column is **not** repeated
//! inside the row — it *is* the tick time ([`with_time`](crate::fluent::StreamOps::with_time)
//! reads it back) — which is also what makes a read → write round trip
//! reproduce the frame instead of doubling the column.
//!
//! Accepted time-column dtypes: `Datetime` (any unit, scaled to nanoseconds;
//! the timezone is ignored, since the physical value is already UTC), `Int64`
//! or `UInt64` (nanoseconds since the epoch). The column is validated **at
//! wiring** — a missing column, an unsupported dtype, a null, a negative value
//! or a decrease is an `Err` naming the row, before the run — so the replay
//! itself cannot fail on ordering. Sort the frame first
//! (`df.sort(["time"], Default::default())`) if it is not already.
//!
//! The file (if any) is read into memory at wiring: polars' Parquet and IPC
//! readers are whole-file, and reading up front is what lets the time column be
//! validated before the run. Rows are then materialised **lazily** over a
//! [`produce_async`] producer as the graph
//! drains, so the frame is never duplicated as a `Vec` of rows; `buffer_size`
//! bounds that look-ahead exactly as it does for `csv_read`.
//! Run with `RunMode::HistoricalFrom(t)` where `t` is at or before the first
//! row's time.
//!
//! # Sink
//!
//! Both sink methods buffer every row of the run — graph time plus values —
//! and build one `DataFrame` in a `stop` hook, at the end of the run: a
//! leading `Datetime[ns]` column of graph times (named by
//! [`PolarsSinkOptions::time_column`], `"time"` by default, or omitted) followed
//! by the rows' columns. [`polars_collect`](PolarsSinkOps::polars_collect)
//! publishes it to its [`PolarsCollector`];
//! [`polars_write`](PolarsSinkOps::polars_write) writes it to a file. At wiring
//! it only probes that the target's directory is writable (creating and
//! removing a scratch file there), so an unwritable path fails before the run
//! without touching `path`. At `stop` the frame is encoded to a sibling temp
//! file (`.<name>.<pid>.<n>.tmp`) and renamed over `path` only once the write
//! succeeded; a failed write removes the temp and leaves `path` as it was. So
//! `path` only ever holds its previous contents or a complete, valid file —
//! never an empty or half-written one.
//!
//! The engine runs `stop` at the end of **every** run, including one a cycle
//! aborted (the [`Op::stop`](crate::op::Op::stop) contract), so an aborted run
//! still yields the rows that reached the sink before the abort: the collector
//! holds them and `polars_write` writes them, as a whole file.
//! The buffer is per-run state, so a second run of the same graph starts empty.
//!
//! Every row must carry the **same column names in the same order** as the
//! first; a column's dtype is fixed by the first row that declares it non-null
//! (a `Null`-typed cell never pins it), and a later conflicting dtype aborts the
//! run naming the column. A row that already has a column named like the time
//! column aborts the run too, rather than writing a frame with two of them.
//!
//! # Deviations
//!
//! There is no legacy twin, so these are departures from the adapter
//! *conventions* rather than from legacy:
//!
//! - **Whole-file read at wiring.** Other file replays (`csv`, `lines`) open at
//!   wiring and read lazily. polars has no incremental reader without its
//!   `lazy` engine (a large dependency), so the file is loaded up front; only
//!   the row materialisation is lazy.
//! - **The sink writes at `stop`, not per tick.** A columnar file is written
//!   once, not appended row by row (a Parquet row group per tick would be
//!   pathological). After an abort both leave the rows seen so far, but here
//!   the write goes through a sibling temp file and a rename, so `path` is
//!   never seen truncated or half-written — only whole files replace it.

use std::cell::RefCell;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow, bail};

pub use ::polars;
pub use ::polars::prelude::{AnyValue, DataFrame, DataType, Schema, SchemaRef, TimeUnit};
use ::polars::prelude::{
    Column, Int64Chunked, IntoColumn, IntoSeries, IpcReader, IpcWriter, ParquetReader,
    ParquetWriter, SerReader, SerWriter, Series,
};

use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, Stream, StreamOps};
use crate::op::{Activation, Tick};
use crate::{Burst, NanoTime, burst};

/// One frame row: the frame's [`Schema`] and one [`AnyValue`] per column.
///
/// The schema is an `Arc` shared by every row of one read, so cloning a row
/// copies a pointer plus its values. See the [module docs](self#rows).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PolarsRow {
    schema: SchemaRef,
    values: Vec<AnyValue<'static>>,
}

impl PolarsRow {
    /// A row of `values` under `schema`, one value per column in schema order.
    ///
    /// Build the `Arc<Schema>` once and share it across rows: the sink compares
    /// consecutive rows' schemas by pointer first, so a shared one is free.
    ///
    /// # Errors
    ///
    /// If `values` does not hold exactly one value per column.
    pub fn new(schema: SchemaRef, values: Vec<AnyValue<'static>>) -> Result<Self> {
        if values.len() != schema.len() {
            bail!(
                "PolarsRow: schema has {} column(s) but {} value(s) were given",
                schema.len(),
                values.len()
            );
        }
        Ok(Self { schema, values })
    }

    /// The row's schema — column names and declared dtypes, in order.
    pub fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// The values, in schema order.
    pub fn values(&self) -> &[AnyValue<'static>] {
        &self.values
    }

    /// The value of column `name`, or `None` if the row has no such column.
    pub fn get(&self, name: &str) -> Option<&AnyValue<'static>> {
        self.schema.index_of(name).map(|i| &self.values[i])
    }
}

/// A columnar file format the adapter reads and writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolarsFormat {
    /// Apache Parquet — `.parquet` / `.pq`.
    Parquet,
    /// Arrow IPC (a.k.a. Feather v2) file format — `.arrow` / `.ipc` / `.feather`.
    Ipc,
}

impl PolarsFormat {
    /// The format named by `path`'s extension (case-insensitive).
    ///
    /// # Errors
    ///
    /// If the extension is missing or not one of the recognised ones.
    pub fn from_path(path: &Path) -> Result<Self> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("parquet" | "pq") => Ok(Self::Parquet),
            Some("arrow" | "ipc" | "feather") => Ok(Self::Ipc),
            _ => bail!(
                "cannot tell the file format of {} from its extension; use .parquet / .pq \
                 for Parquet or .arrow / .ipc / .feather for Arrow IPC, or name the format \
                 explicitly",
                path.display()
            ),
        }
    }

    /// Read the whole file at `path` into a `DataFrame`.
    ///
    /// # Errors
    ///
    /// If the file cannot be opened or decoded.
    pub fn read(self, path: &Path) -> Result<DataFrame> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        match self {
            Self::Parquet => ParquetReader::new(file).finish(),
            Self::Ipc => IpcReader::new(file).finish(),
        }
        .with_context(|| format!("decoding {self:?} file {}", path.display()))
    }

    /// Write `df` to `path` in this format, creating or truncating it.
    ///
    /// # Errors
    ///
    /// If the file cannot be created or the frame cannot be encoded.
    pub fn write(self, path: &Path, df: &mut DataFrame) -> Result<()> {
        let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
        match self {
            Self::Parquet => ParquetWriter::new(file).finish(df).map(|_| ()),
            Self::Ipc => IpcWriter::new(file).finish(df),
        }
        .with_context(|| format!("encoding {self:?} file {}", path.display()))
    }
}

/// What [`polars_read`] replays. Every variant converts via `From`, so a call
/// site passes a `DataFrame` or a path directly.
#[derive(Debug, Clone)]
pub enum PolarsSource {
    /// An in-memory frame.
    Frame(DataFrame),
    /// A file whose format is inferred from its extension
    /// ([`PolarsFormat::from_path`]).
    Path(PathBuf),
    /// A Parquet file, whatever its extension.
    Parquet(PathBuf),
    /// An Arrow IPC file, whatever its extension.
    Ipc(PathBuf),
}

impl PolarsSource {
    /// Load the frame (reading the file, if it is one) — the wiring-time I/O.
    fn load(self) -> Result<DataFrame> {
        let (path, format) = match self {
            Self::Frame(df) => return Ok(df),
            Self::Path(p) => {
                let format = PolarsFormat::from_path(&p)?;
                (p, format)
            }
            Self::Parquet(p) => (p, PolarsFormat::Parquet),
            Self::Ipc(p) => (p, PolarsFormat::Ipc),
        };
        format.read(&path)
    }
}

impl From<DataFrame> for PolarsSource {
    fn from(df: DataFrame) -> Self {
        Self::Frame(df)
    }
}

impl From<PathBuf> for PolarsSource {
    fn from(p: PathBuf) -> Self {
        Self::Path(p)
    }
}

impl From<&PathBuf> for PolarsSource {
    fn from(p: &PathBuf) -> Self {
        Self::Path(p.clone())
    }
}

impl From<&Path> for PolarsSource {
    fn from(p: &Path) -> Self {
        Self::Path(p.to_path_buf())
    }
}

impl From<&str> for PolarsSource {
    fn from(p: &str) -> Self {
        Self::Path(PathBuf::from(p))
    }
}

impl From<String> for PolarsSource {
    fn from(p: String) -> Self {
        Self::Path(PathBuf::from(p))
    }
}

/// Deterministic historical replay of a polars frame: each row is emitted as a
/// [`PolarsRow`] at its `time_column` instant, rows sharing an instant grouped
/// into one atomic [`Burst`]. The time column itself is dropped from the rows.
///
/// `source` is a `DataFrame` or a Parquet / IPC path (see [`PolarsSource`]).
/// `buffer_size` bounds the replay's look-ahead (`None` = unbounded), as for
/// `csv_read`. Run the graph with
/// `RunMode::HistoricalFrom(t)`, `t` at or before the first row's time.
///
/// # Errors
///
/// At **wiring**, if the file cannot be read, `time_column` is missing or not
/// `Datetime` / `Int64` / `UInt64`, or any time is null, negative or smaller
/// than the one before it (the error names the row). A value that cannot be
/// read out of the frame during the run aborts it with context.
pub fn polars_read(
    g: &GraphBuilder,
    source: impl Into<PolarsSource>,
    time_column: &str,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<PolarsRow>>> {
    let source = source.into();
    let what = match &source {
        PolarsSource::Frame(_) => "the frame".to_string(),
        PolarsSource::Path(p) | PolarsSource::Parquet(p) | PolarsSource::Ipc(p) => {
            p.display().to_string()
        }
    };
    let df = source
        .load()
        .with_context(|| format!("polars_read: reading {what}"))?;
    let time = df.column(time_column).map_err(|_| {
        anyhow!(
            "polars_read: no time column '{time_column}' in {what}; its columns are {:?}",
            df.get_column_names()
        )
    })?;
    let times =
        time_nanos(time).with_context(|| format!("polars_read: time column '{time_column}'"))?;
    let data = df
        .drop(time_column)
        .with_context(|| format!("polars_read: dropping '{time_column}'"))?;
    let schema = data.schema().clone();

    produce_async(
        g,
        move |_p: RunParams| async move {
            Ok(async_stream::stream! {
                for (i, time) in times.into_iter().enumerate() {
                    match row_at(&data, &schema, i) {
                        Ok(row) => yield Ok((time, row)),
                        Err(e) => {
                            yield Err(e.context(format!("polars_read: reading row {i}")));
                            break;
                        }
                    }
                }
            })
        },
        buffer_size,
    )
}

/// The time column as graph instants, validated: no nulls, no negatives,
/// non-decreasing.
fn time_nanos(column: &Column) -> Result<Vec<NanoTime>> {
    let raw: Vec<Option<i128>> = match column.dtype() {
        DataType::Datetime(unit, _) => {
            let scale: i128 = match unit {
                TimeUnit::Nanoseconds => 1,
                TimeUnit::Microseconds => 1_000,
                TimeUnit::Milliseconds => 1_000_000,
            };
            let physical = column.cast(&DataType::Int64)?;
            let ca = physical.i64()?;
            (0..ca.len())
                .map(|i| ca.get(i).map(|v| i128::from(v) * scale))
                .collect()
        }
        DataType::Int64 => {
            let ca = column.i64()?;
            (0..ca.len()).map(|i| ca.get(i).map(i128::from)).collect()
        }
        DataType::UInt64 => {
            let ca = column.u64()?;
            (0..ca.len()).map(|i| ca.get(i).map(i128::from)).collect()
        }
        other => bail!(
            "has dtype {other}; supported are Datetime (any unit), Int64 and UInt64 \
             (nanoseconds since the epoch)"
        ),
    };
    let mut times = Vec::with_capacity(raw.len());
    let mut previous = NanoTime::ZERO;
    for (i, value) in raw.into_iter().enumerate() {
        let value = value.ok_or_else(|| anyhow!("row {i} has a null time"))?;
        let nanos = u64::try_from(value).map_err(|_| {
            anyhow!("row {i} has time {value}ns, which is negative or past the NanoTime range")
        })?;
        let time = NanoTime::new(nanos);
        if time < previous {
            bail!(
                "row {i} time {nanos}ns is before row {} time {}ns; the time column must be \
                 non-decreasing (sort the frame by it first)",
                i - 1,
                u64::from(previous)
            );
        }
        times.push(time);
        previous = time;
    }
    Ok(times)
}

/// Materialise row `i` of `data`.
fn row_at(data: &DataFrame, schema: &SchemaRef, i: usize) -> Result<PolarsRow> {
    let values = data
        .columns()
        .iter()
        .map(|c| c.get(i).map(AnyValue::into_static))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(PolarsRow {
        schema: schema.clone(),
        values,
    })
}

/// Options shared by the sink methods. `Default` is the compatibility
/// surface: a leading `"time"` column, and the file format taken from the
/// path's extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolarsSinkOptions {
    /// Name of the leading `Datetime[ns]` column of graph times, or `None` to
    /// omit it. Default `Some("time")`.
    pub time_column: Option<String>,
    /// File format for [`polars_write`](PolarsSinkOps::polars_write); `None`
    /// infers it from the extension ([`PolarsFormat::from_path`]). Ignored by
    /// [`polars_collect`](PolarsSinkOps::polars_collect). Default `None`.
    pub format: Option<PolarsFormat>,
}

impl Default for PolarsSinkOptions {
    fn default() -> Self {
        Self {
            time_column: Some("time".to_string()),
            format: None,
        }
    }
}

/// The read side of [`polars_collect`](PolarsSinkOps::polars_collect): holds
/// the frame built by the most recent run that ended normally.
///
/// Graph-thread-local (`Rc`), like the graph itself.
#[derive(Debug, Clone, Default)]
pub struct PolarsCollector {
    frame: Rc<RefCell<Option<DataFrame>>>,
}

impl PolarsCollector {
    /// The collected frame, or `None` before any run has completed. A cheap
    /// clone: polars columns are reference-counted.
    pub fn frame(&self) -> Option<DataFrame> {
        self.frame.borrow().clone()
    }
}

/// DataFrame / Parquet / IPC sinks — the outbound counterpart of
/// [`polars_read`]. See the [module docs](self#sink) for the frame's shape and
/// the per-row schema rules.
pub trait PolarsSinkOps {
    /// Collect every row of the run into a `DataFrame`, readable after the run
    /// from the returned [`PolarsCollector`]. Default [`PolarsSinkOptions`].
    fn polars_collect(&self) -> (Stream<()>, PolarsCollector) {
        self.polars_collect_with_options(PolarsSinkOptions::default())
    }

    /// [`polars_collect`](Self::polars_collect) with explicit options.
    fn polars_collect_with_options(
        &self,
        options: PolarsSinkOptions,
    ) -> (Stream<()>, PolarsCollector);

    /// Collect every row of the run and write the frame to `path` at the end of
    /// the run (after an abort too, with the rows seen so far). The format
    /// comes from the extension. Returns the sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// At wiring, if the format cannot be inferred or `path`'s directory is not
    /// writable (probed with a scratch file; `path` itself is not touched). A
    /// failed encode or write at the end of the run fails the run and leaves
    /// `path` as it was — the frame is written to a sibling temp file and
    /// renamed over `path` only on success.
    fn polars_write(&self, path: impl AsRef<Path>) -> Result<Stream<()>> {
        self.polars_write_with_options(path, PolarsSinkOptions::default())
    }

    /// [`polars_write`](Self::polars_write) with explicit options.
    ///
    /// # Errors
    ///
    /// As [`polars_write`](Self::polars_write).
    fn polars_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: PolarsSinkOptions,
    ) -> Result<Stream<()>>;
}

impl PolarsSinkOps for Stream<Burst<PolarsRow>> {
    fn polars_collect_with_options(
        &self,
        options: PolarsSinkOptions,
    ) -> (Stream<()>, PolarsCollector) {
        let collector = PolarsCollector::default();
        let slot = collector.frame.clone();
        let sink = self.frame_sink("polars_collect", options.time_column, move |df| {
            *slot.borrow_mut() = Some(df);
            Ok(())
        });
        (sink, collector)
    }

    fn polars_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: PolarsSinkOptions,
    ) -> Result<Stream<()>> {
        let path = path.as_ref().to_path_buf();
        let format = match options.format {
            Some(f) => f,
            None => PolarsFormat::from_path(&path).context("polars_write")?,
        };
        probe_writable(&path).context("polars_write")?;
        Ok(
            self.frame_sink("polars_write", options.time_column, move |mut df| {
                write_atomic(format, &path, &mut df)
                    .context("polars_write: writing the collected frame")
            }),
        )
    }
}

/// A sibling of `path` to write through: `.<name>.<pid>.<n>.tmp` in the same
/// directory, so the final rename never crosses a filesystem. The pid keeps
/// parallel processes apart, the counter parallel sinks in one process.
fn temp_sibling(path: &Path) -> Result<PathBuf> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("{} names no file", path.display()))?;
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut tmp = std::ffi::OsString::from(".");
    tmp.push(name);
    tmp.push(format!(".{}.{n}.tmp", std::process::id()));
    Ok(path.with_file_name(tmp))
}

/// The wiring-time check: `path` is not a directory, and a file can be created
/// beside it (which is all the final rename needs). `path` itself is untouched.
fn probe_writable(path: &Path) -> Result<()> {
    if path.is_dir() {
        bail!("{} is a directory", path.display());
    }
    let tmp = temp_sibling(path)?;
    File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    fs::remove_file(&tmp).with_context(|| format!("removing {}", tmp.display()))
}

/// Encode `df` to a temp sibling, then rename it over `path`. On any failure
/// the temp is removed and `path` keeps whatever it held before.
fn write_atomic(format: PolarsFormat, path: &Path, df: &mut DataFrame) -> Result<()> {
    let tmp = temp_sibling(path)?;
    let written = format.write(&tmp, df).and_then(|()| {
        fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} over {}", tmp.display(), path.display()))
    });
    if written.is_err() {
        // Best effort: the write error is the one worth reporting.
        let _ = fs::remove_file(&tmp);
    }
    written
}

/// Single-value convenience: a plain `Stream<PolarsRow>` sinks each value as a
/// one-row burst.
impl PolarsSinkOps for Stream<PolarsRow> {
    fn polars_collect_with_options(
        &self,
        options: PolarsSinkOptions,
    ) -> (Stream<()>, PolarsCollector) {
        self.map(|r: &PolarsRow| -> Burst<PolarsRow> { burst![r.clone()] })
            .polars_collect_with_options(options)
    }

    fn polars_write_with_options(
        &self,
        path: impl AsRef<Path>,
        options: PolarsSinkOptions,
    ) -> Result<Stream<()>> {
        self.map(|r: &PolarsRow| -> Burst<PolarsRow> { burst![r.clone()] })
            .polars_write_with_options(path, options)
    }
}

impl Stream<Burst<PolarsRow>> {
    /// The shared sink: buffer every row per cycle, build the frame at `stop`
    /// and hand it to `finish`.
    fn frame_sink<F>(&self, who: &'static str, time_column: Option<String>, finish: F) -> Stream<()>
    where
        F: FnMut(DataFrame) -> Result<()> + 'static,
    {
        self.wire(move |b, h| {
            b.register_op1_with_stop(
                h,
                who,
                Activation::NONE,
                (time_column, finish),
                FrameBuffer::default,
                move |(time_column, _), buffer: &mut FrameBuffer, burst: &Burst<PolarsRow>, ctx| {
                    for row in burst {
                        buffer
                            .push(ctx.time(), row, time_column.as_deref())
                            .with_context(|| format!("{who}: row at {}", ctx.time()))?;
                    }
                    Ok(Tick::Value(()))
                },
                move |(time_column, finish), buffer: &mut FrameBuffer, _ctx| {
                    let df = buffer
                        .finish(time_column.as_deref())
                        .with_context(|| format!("{who}: building the frame"))?;
                    finish(df)
                },
            )
        })
    }
}

/// The sink's per-run state: graph times plus one value column per row column.
#[derive(Default)]
struct FrameBuffer {
    /// The last row schema seen — a pointer-equal row skips the checks.
    schema: Option<SchemaRef>,
    /// Each column's dtype: the first non-`Null` one declared.
    dtypes: Vec<DataType>,
    times: Vec<i64>,
    columns: Vec<Vec<AnyValue<'static>>>,
}

impl FrameBuffer {
    fn push(&mut self, time: NanoTime, row: &PolarsRow, time_column: Option<&str>) -> Result<()> {
        match &self.schema {
            None => {
                if let Some(name) = time_column
                    && row.schema.contains(name)
                {
                    bail!(
                        "the row already has a column named '{name}', which the sink adds for \
                         graph time; set PolarsSinkOptions::time_column to another name or None"
                    );
                }
                self.dtypes = row.schema.iter_values().cloned().collect();
                self.columns = vec![Vec::new(); row.schema.len()];
            }
            Some(seen) if Arc::ptr_eq(seen, &row.schema) => {}
            Some(seen) => {
                if !seen.iter_names().eq(row.schema.iter_names()) {
                    bail!(
                        "row columns {:?} differ from the first row's {:?}",
                        row.schema.iter_names().collect::<Vec<_>>(),
                        seen.iter_names().collect::<Vec<_>>()
                    );
                }
                for ((name, dtype), pinned) in row.schema.iter().zip(self.dtypes.iter_mut()) {
                    if dtype.is_null() {
                        continue;
                    }
                    if pinned.is_null() {
                        *pinned = dtype.clone();
                    } else if pinned != dtype {
                        bail!("column '{name}' changed dtype from {pinned} to {dtype}");
                    }
                }
            }
        }
        self.schema = Some(row.schema.clone());
        self.times.push(
            i64::try_from(u64::from(time)).context("graph time is past the Datetime[ns] range")?,
        );
        for (column, value) in self.columns.iter_mut().zip(&row.values) {
            column.push(value.clone());
        }
        Ok(())
    }

    /// Build the frame and reset for the next run.
    fn finish(&mut self, time_column: Option<&str>) -> Result<DataFrame> {
        let buffer = std::mem::take(self);
        let height = buffer.times.len();
        let mut columns = Vec::with_capacity(buffer.columns.len() + 1);
        if let Some(name) = time_column {
            let time = Int64Chunked::from_vec(name.into(), buffer.times)
                .into_datetime(TimeUnit::Nanoseconds, None)
                .into_series()
                .into_column();
            columns.push(time);
        }
        if let Some(schema) = buffer.schema {
            for ((name, values), dtype) in
                schema.iter_names().zip(buffer.columns).zip(buffer.dtypes)
            {
                let series = Series::from_any_values_and_dtype(name.clone(), &values, &dtype, true)
                    .with_context(|| format!("column '{name}' as {dtype}"))?;
                columns.push(series.into_column());
            }
        }
        Ok(DataFrame::new(height, columns)?)
    }
}
