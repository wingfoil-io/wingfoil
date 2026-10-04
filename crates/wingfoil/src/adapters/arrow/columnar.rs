//! The format-agnostic columnar core behind [`arrow`](crate::adapters::arrow):
//! everything a serde-typed, batch-oriented file adapter needs that is not the
//! file format itself. The Arrow IPC adapter is its first user; a Parquet
//! adapter is meant to be the second, supplying only a [`BatchFileWriter`], an
//! opener for [`replay_batch_files`] and its own options.
//!
//! - **Schema** — [`trace_fields`]: `T`'s Arrow fields, traced from its serde
//!   shape at wiring.
//! - **Batching** — [`BatchBuilder`]: rows pushed one record at a time into a
//!   `serde_arrow` builder plus an optional leading graph-time column, cut into
//!   a [`RecordBatch`] on demand.
//! - **Partitioning** — [`TimePartition`] (public, re-exported by the adapter)
//!   and the private `PartitionKey` routing.
//! - **Sink wiring** — [`wire_batch_sink`]: the `with_time` → `for_each` →
//!   `finally` sink over any [`BatchFileWriter`], single-file or partitioned.
//! - **Replay** — [`input_files`] (a file, or a directory walked for one
//!   extension) and [`replay_batch_files`]: the lazy multi-file
//!   [`produce_async`] replay over "open a file → iterator of record batches".
//!
//! Everything but [`TimePartition`] is `pub(crate)`: the shapes are tuned to
//! the two in-tree adapters, not offered as a public extension point.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use anyhow::{Context, Result};
use arrow_array::{ArrayRef, RecordBatch, RecordBatchOptions, TimestampNanosecondArray};
use arrow_schema::{ArrowError, DataType, Field, FieldRef, Schema, SchemaRef, TimeUnit};
use chrono::{Datelike, NaiveDateTime, Timelike};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_arrow::ArrayBuilder;
use serde_arrow::schema::{SchemaLike, TracingOptions};
use wingfoil::NanoTime;

use crate::Burst;
use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, Stream, StreamOps};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// `T`'s Arrow fields, traced from its serde shape without samples.
///
/// `T` must be a struct with named fields; a type `serde_arrow` cannot trace
/// from its type alone (a primitive, a tuple, an untagged enum) is an error
/// naming the type.
pub(crate) fn trace_fields<T: DeserializeOwned>(adapter: &str) -> Result<Vec<FieldRef>> {
    Vec::<FieldRef>::from_type::<T>(TracingOptions::default()).with_context(|| {
        format!(
            "{adapter}: cannot trace an Arrow schema for `{}` (records must be structs with named fields)",
            std::any::type_name::<T>()
        )
    })
}

// ---------------------------------------------------------------------------
// Batching
// ---------------------------------------------------------------------------

/// Rows pending in Arrow form: `T`'s columns in a `serde_arrow` builder (one
/// record pushed at a time — no intermediate `Vec<T>`) plus, when configured, a
/// leading graph-time column. [`take_batch`](Self::take_batch) cuts them into
/// one [`RecordBatch`] and resets.
pub(crate) struct BatchBuilder<T> {
    schema: SchemaRef,
    /// `T`'s fields, kept to rebuild `probe`.
    fields: Vec<FieldRef>,
    builder: ArrayBuilder,
    /// Every record is serialized here first. `serde_arrow` has no rollback: a
    /// record that fails part-way leaves the fields it already appended, so the
    /// columns no longer line up and the whole builder is lost. Probing first
    /// means a bad record never touches `builder`, and the rows before it still
    /// reach the file. Rebuilt on failure and at every
    /// [`take_batch`](Self::take_batch), so it holds at most one batch.
    probe: ArrayBuilder,
    /// The leading time column's values (nanoseconds since the epoch), one per
    /// pending row; `None` when the sink writes no time column.
    times: Option<Vec<i64>>,
    pending: usize,
    batch_size: usize,
    _record: PhantomData<fn(&T)>,
}

impl<T: Serialize + DeserializeOwned> BatchBuilder<T> {
    /// Trace `T`'s schema and prepare an empty builder. The batch schema is
    /// `time_column` (a non-null `Timestamp(Nanosecond, None)`) when given,
    /// then `T`'s fields. A `batch_size` of zero is treated as one.
    pub(crate) fn new(adapter: &str, time_column: Option<&str>, batch_size: usize) -> Result<Self> {
        let fields = trace_fields::<T>(adapter)?;
        let builder = ArrayBuilder::from_arrow(&fields)
            .with_context(|| format!("{adapter}: building the Arrow array builder"))?;
        let probe = ArrayBuilder::from_arrow(&fields)
            .with_context(|| format!("{adapter}: building the Arrow array builder"))?;
        let mut all: Vec<FieldRef> = Vec::with_capacity(fields.len() + 1);
        if let Some(name) = time_column {
            all.push(Arc::new(Field::new(
                name,
                DataType::Timestamp(TimeUnit::Nanosecond, None),
                false,
            )));
        }
        all.extend(fields.iter().cloned());
        Ok(Self {
            schema: Arc::new(Schema::new(all)),
            fields,
            builder,
            probe,
            times: time_column.map(|_| Vec::new()),
            pending: 0,
            batch_size: batch_size.max(1),
            _record: PhantomData,
        })
    }

    /// Rows pushed since the last [`take_batch`](Self::take_batch).
    pub(crate) fn pending(&self) -> usize {
        self.pending
    }

    /// The schema of every batch this builder cuts.
    pub(crate) fn schema(&self) -> &SchemaRef {
        &self.schema
    }

    /// Append one record stamped with graph time `time`. Returns `true` once
    /// `batch_size` rows are pending — the caller's cue to
    /// [`take_batch`](Self::take_batch).
    ///
    /// A record that fails to serialize is an error and leaves the pending rows
    /// intact: they still form a valid batch.
    pub(crate) fn push(&mut self, time: NanoTime, record: &T) -> Result<bool> {
        let nanos = match self.times {
            Some(_) => Some(i64::try_from(u64::from(time)).with_context(|| {
                format!("graph time {time} does not fit a nanosecond Timestamp column")
            })?),
            None => None,
        };
        if let Err(e) = self.probe.push(record) {
            self.probe = ArrayBuilder::from_arrow(&self.fields)
                .context("rebuilding the Arrow array builder")?;
            return Err(anyhow::Error::new(e).context("failed to serialize record"));
        }
        // Serialization is deterministic, so the probe passing means this does.
        self.builder
            .push(record)
            .context("failed to serialize record")?;
        if let (Some(times), Some(nanos)) = (&mut self.times, nanos) {
            times.push(nanos);
        }
        self.pending += 1;
        Ok(self.pending >= self.batch_size)
    }

    /// Cut the pending rows into one batch and reset; `None` when nothing is
    /// pending.
    pub(crate) fn take_batch(&mut self) -> Result<Option<RecordBatch>> {
        if self.pending == 0 {
            return Ok(None);
        }
        let rows = std::mem::take(&mut self.pending);
        self.probe =
            ArrayBuilder::from_arrow(&self.fields).context("rebuilding the Arrow array builder")?;
        let mut columns: Vec<ArrayRef> = Vec::with_capacity(self.schema.fields().len());
        if let Some(times) = &mut self.times {
            columns.push(Arc::new(TimestampNanosecondArray::from(std::mem::take(
                times,
            ))));
        }
        columns.extend(
            self.builder
                .to_arrow()
                .context("failed to build Arrow arrays")?,
        );
        // An explicit row count keeps a column-less schema (a field-less `T`
        // with no time column) valid.
        let options = RecordBatchOptions::new().with_row_count(Some(rows));
        RecordBatch::try_new_with_options(self.schema.clone(), columns, &options)
            .map(Some)
            .context("failed to assemble record batch")
    }
}

// ---------------------------------------------------------------------------
// Partitioning
// ---------------------------------------------------------------------------

/// Granularity of a Hive-partitioned write. Each variant's directory extends
/// the previous one's (`year=`, then `month=`, then `day=`, then `hour=`),
/// zero-padded so lexical order is time order:
///
/// ```text
/// root/year=2026/month=10/day=04/hour=09/<file>   # Hour
/// root/year=2026/month=10/day=04/<file>           # Day
/// root/year=2026/month=10/<file>                  # Month
/// root/year=2026/<file>                           # Year
/// ```
///
/// Keys are the row's graph time in **UTC**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TimePartition {
    /// `year=YYYY/`
    Year,
    /// `year=YYYY/month=MM/`
    Month,
    /// `year=YYYY/month=MM/day=DD/`
    Day,
    /// `year=YYYY/month=MM/day=DD/hour=HH/`
    Hour,
}

/// The UTC calendar fields a graph time falls into at one granularity — the
/// identity of a partition. Fields finer than the granularity are zeroed, so
/// two times in one partition compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PartitionKey {
    partition: TimePartition,
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
}

impl PartitionKey {
    fn of(time: NanoTime, partition: TimePartition) -> Self {
        let dt = NaiveDateTime::from(time);
        Self {
            partition,
            year: dt.year(),
            month: if partition >= TimePartition::Month {
                dt.month()
            } else {
                0
            },
            day: if partition >= TimePartition::Day {
                dt.day()
            } else {
                0
            },
            hour: if partition >= TimePartition::Hour {
                dt.hour()
            } else {
                0
            },
        }
    }

    /// The partition's directory relative to the root: nested, zero-padded
    /// `key=value` segments.
    fn relative_dir(&self) -> PathBuf {
        let mut dir = PathBuf::from(format!("year={:04}", self.year));
        if self.partition >= TimePartition::Month {
            dir.push(format!("month={:02}", self.month));
        }
        if self.partition >= TimePartition::Day {
            dir.push(format!("day={:02}", self.day));
        }
        if self.partition >= TimePartition::Hour {
            dir.push(format!("hour={:02}", self.hour));
        }
        dir
    }
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// One output file in some batch-oriented format. The core owns batching,
/// routing and the error context (adapter name + path); an implementor only
/// encodes.
pub(crate) trait BatchFileWriter: Sized {
    /// Format-specific knobs, cloned into the sink and handed to every
    /// [`create`](Self::create) (a partitioned sink creates many files).
    type Options: 'static;

    /// Create (truncating) `path` and write whatever header the format puts
    /// before the first batch.
    fn create(path: &Path, schema: &SchemaRef, options: &Self::Options) -> Result<Self>;

    /// Append one batch. Whether it reaches the OS now (Arrow IPC: yes, so the
    /// file is tailable) or is buffered into a larger unit (Parquet row
    /// groups) is the format's call.
    fn write(&mut self, batch: &RecordBatch) -> Result<()>;

    /// Write the format's trailer (end-of-stream marker, footer) and flush.
    fn finish(self) -> Result<()>;
}

/// Where a batch sink's rows go.
pub(crate) enum SinkTarget {
    /// One file, created at wiring.
    File(PathBuf),
    /// A Hive-partitioned tree under `root`, one `file_name` per partition,
    /// created as graph time reaches it.
    Partitioned {
        root: PathBuf,
        partition: TimePartition,
        file_name: String,
    },
}

/// The format-independent sink knobs.
pub(crate) struct SinkConfig {
    /// Rows per batch handed to the writer, at most.
    pub(crate) batch_size: usize,
    /// Name of the leading graph-time column; `None` writes `T`'s columns only.
    pub(crate) time_column: Option<String>,
    /// Hand the pending rows to the writer at the end of every tick, rather
    /// than only when `batch_size` fills (and at teardown).
    pub(crate) flush_every_tick: bool,
}

/// Wire a batch-file sink onto `stream`: `with_time` → `for_each` (batch and
/// route each burst) → `finally` (write what is pending and finish the open
/// file — at teardown, after a clean run *or* an aborted one). The state lives
/// in an `Rc<RefCell<_>>` shared by the two closures: graph-thread-local, no
/// lock.
///
/// Fails at wiring if `T`'s schema cannot be traced, the single file cannot be
/// created, or a partitioned root cannot be created.
pub(crate) fn wire_batch_sink<T, W>(
    stream: &Stream<Burst<T>>,
    adapter: &'static str,
    target: SinkTarget,
    config: SinkConfig,
    options: W::Options,
) -> Result<Stream<()>>
where
    T: Serialize + DeserializeOwned + Clone + Default + 'static,
    W: BatchFileWriter + 'static,
{
    let batch = BatchBuilder::<T>::new(adapter, config.time_column.as_deref(), config.batch_size)?;
    let mut sink = BatchSink::<T, W> {
        adapter,
        batch,
        options,
        open: None,
        router: None,
        flush_every_tick: config.flush_every_tick,
    };
    match target {
        SinkTarget::File(path) => sink.open_file(path)?,
        SinkTarget::Partitioned {
            root,
            partition,
            file_name,
        } => {
            std::fs::create_dir_all(&root)
                .with_context(|| format!("{adapter}: failed to create {}", root.display()))?;
            sink.router = Some(Router {
                root,
                partition,
                file_name,
                current: None,
            });
        }
    }
    let state = Rc::new(RefCell::new(sink));
    let per_tick = state.clone();
    let written = stream
        .with_time()
        .for_each(move |(time, burst): &(NanoTime, Burst<T>)| {
            per_tick.borrow_mut().on_tick(*time, burst)
        });
    Ok(written.finally(move |_| state.borrow_mut().close_file()))
}

/// Routes rows to the partition file for their graph time.
struct Router {
    root: PathBuf,
    partition: TimePartition,
    file_name: String,
    /// The partition whose file is open; `None` before the first row.
    current: Option<PartitionKey>,
}

/// The file currently being written.
struct OpenFile<W> {
    path: PathBuf,
    writer: W,
}

/// A batch sink's graph-thread state.
struct BatchSink<T, W: BatchFileWriter> {
    adapter: &'static str,
    batch: BatchBuilder<T>,
    options: W::Options,
    /// `None` before the first partitioned row, between a failed partition
    /// switch and teardown, and after the final close — so a `finally` after
    /// an aborted tick never finishes a file twice.
    open: Option<OpenFile<W>>,
    /// `None` for a single-file sink.
    router: Option<Router>,
    flush_every_tick: bool,
}

impl<T, W> BatchSink<T, W>
where
    T: Serialize + DeserializeOwned + Default,
    W: BatchFileWriter,
{
    fn open_file(&mut self, path: PathBuf) -> Result<()> {
        let writer = W::create(&path, self.batch.schema(), &self.options)
            .with_context(|| format!("{}: failed to create {}", self.adapter, path.display()))?;
        self.open = Some(OpenFile { path, writer });
        Ok(())
    }

    fn on_tick(&mut self, time: NanoTime, burst: &Burst<T>) -> Result<()> {
        if burst.is_empty() {
            return Ok(());
        }
        self.route(time)?;
        for record in burst.iter() {
            let full = self.batch.push(time, record).with_context(|| {
                format!("{}: writing to {}", self.adapter, self.open_path_display())
            })?;
            if full {
                self.write_pending()?;
            }
        }
        if self.flush_every_tick {
            self.write_pending()?;
        }
        Ok(())
    }

    /// For a partitioned sink, make the file for `time`'s partition the open
    /// one, finishing the previous partition's file first. Graph time is
    /// monotonic, so a partition once left is never revisited.
    fn route(&mut self, time: NanoTime) -> Result<()> {
        let Some(router) = &self.router else {
            return Ok(());
        };
        let key = PartitionKey::of(time, router.partition);
        if router.current == Some(key) {
            return Ok(());
        }
        let dir = router.root.join(key.relative_dir());
        let path = dir.join(&router.file_name);
        self.close_file()?;
        std::fs::create_dir_all(&dir).with_context(|| {
            format!(
                "{}: failed to create partition {}",
                self.adapter,
                dir.display()
            )
        })?;
        self.open_file(path)?;
        if let Some(router) = &mut self.router {
            router.current = Some(key);
        }
        Ok(())
    }

    /// Hand the pending rows to the open file as one batch.
    fn write_pending(&mut self) -> Result<()> {
        let adapter = self.adapter;
        let Some(file) = self.open.as_mut() else {
            if self.batch.pending() == 0 {
                return Ok(());
            }
            anyhow::bail!(
                "{adapter}: {} pending rows but no open file (an earlier error closed it)",
                self.batch.pending()
            );
        };
        let context = || format!("{adapter}: writing to {}", file.path.display());
        let Some(batch) = self.batch.take_batch().with_context(context)? else {
            return Ok(());
        };
        file.writer.write(&batch).with_context(context)
    }

    /// Write what is pending and finish the open file, if any. Idempotent.
    fn close_file(&mut self) -> Result<()> {
        if self.open.is_none() {
            return Ok(());
        }
        let pending = self.write_pending();
        let file = self
            .open
            .take()
            .expect("invariant: checked open above, and write_pending never closes it");
        let finished = file
            .writer
            .finish()
            .with_context(|| format!("{}: failed to finish {}", self.adapter, file.path.display()));
        pending.and(finished)
    }

    fn open_path_display(&self) -> String {
        self.open.as_ref().map_or_else(
            || "<no open file>".to_owned(),
            |f| f.path.display().to_string(),
        )
    }
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

/// The files a replay reads, grouped by directory: `path` itself when it is
/// not a directory (whether it exists is the opener's to report), or every
/// `*.{extension}` file under it, recursively. Groups are ordered by directory
/// path — which for a zero-padded Hive tree is time order — and files within a
/// group by name. Anything else in the tree (`_SUCCESS` markers, sidecars) is
/// ignored.
///
/// Files sharing a directory are one group because they share a partition (two
/// runs into one root under different `file_name`s, say): their rows
/// interleave in time, so [`replay_batch_files`] merges them rather than
/// replaying one after another.
///
/// Fails if the directory cannot be read or holds no matching file.
pub(crate) fn input_files(
    path: &Path,
    extension: &str,
    adapter: &str,
) -> Result<Vec<Vec<PathBuf>>> {
    if !path.is_dir() {
        return Ok(vec![vec![path.to_path_buf()]]);
    }
    let mut files = Vec::new();
    walk(path, extension, adapter, &mut files)?;
    if files.is_empty() {
        anyhow::bail!("{adapter}: no .{extension} files under {}", path.display());
    }
    let mut groups: BTreeMap<PathBuf, Vec<PathBuf>> = BTreeMap::new();
    for file in files {
        let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
        groups.entry(dir).or_default().push(file);
    }
    Ok(groups
        .into_values()
        .map(|mut group| {
            group.sort();
            group
        })
        .collect())
}

fn walk(dir: &Path, extension: &str, adapter: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    let context = || format!("{adapter}: failed to read directory {}", dir.display());
    for entry in std::fs::read_dir(dir).with_context(context)? {
        let path = entry.with_context(context)?.path();
        if path.is_dir() {
            walk(&path, extension, adapter, out)?;
        } else if path.extension().is_some_and(|e| e == extension) {
            out.push(path);
        }
    }
    Ok(())
}

/// One file being replayed: its batch reader and the rows of the batch it is
/// draining.
struct Cursor<T, R> {
    path: PathBuf,
    reader: R,
    rows: std::vec::IntoIter<T>,
}

impl<T, R> Cursor<T, R>
where
    T: DeserializeOwned,
    R: Iterator<Item = std::result::Result<RecordBatch, ArrowError>>,
{
    /// The file's next row, decoding the next batch when the current one is
    /// drained; `None` at end of file.
    fn next_row(&mut self, adapter: &str) -> Result<Option<T>> {
        loop {
            if let Some(row) = self.rows.next() {
                return Ok(Some(row));
            }
            let Some(batch) = self.reader.next() else {
                return Ok(None);
            };
            let display = self.path.display();
            let batch = batch.with_context(|| {
                format!("{adapter}: failed to decode a record batch from {display}")
            })?;
            self.rows = serde_arrow::from_record_batch::<Vec<T>>(&batch)
                .with_context(|| format!("{adapter}: failed to deserialize rows from {display}"))?
                .into_iter();
        }
    }
}

/// The open files of one group, each with its next row (`None` once drained)
/// stamped with its time.
struct Merge<T, R> {
    cursors: Vec<Cursor<T, R>>,
    heads: Vec<Option<(NanoTime, T)>>,
}

impl<T, R> Merge<T, R>
where
    T: DeserializeOwned,
    R: Iterator<Item = std::result::Result<RecordBatch, ArrowError>>,
{
    fn open<O>(paths: Vec<PathBuf>, open: &O) -> Result<Self>
    where
        O: Fn(&Path) -> Result<R>,
    {
        let cursors = paths
            .into_iter()
            .map(|path| {
                open(&path).map(|reader| Cursor {
                    path,
                    reader,
                    rows: Vec::new().into_iter(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let heads = cursors.iter().map(|_| None).collect();
        Ok(Self { cursors, heads })
    }

    /// Read every file's first row (a decode error surfaces here, on the
    /// producer task, not at wiring).
    fn prime<F: Fn(&T) -> NanoTime>(&mut self, adapter: &str, get_time: &F) -> Result<()> {
        for i in 0..self.cursors.len() {
            self.advance(i, adapter, get_time)?;
        }
        Ok(())
    }

    fn advance<F: Fn(&T) -> NanoTime>(
        &mut self,
        i: usize,
        adapter: &str,
        get_time: &F,
    ) -> Result<()> {
        self.heads[i] = self.cursors[i]
            .next_row(adapter)?
            .map(|row| (get_time(&row), row));
        Ok(())
    }

    /// The earliest pending row across the group — on a tie, the file that
    /// sorts first — or `None` when every file is drained.
    fn next<F: Fn(&T) -> NanoTime>(
        &mut self,
        adapter: &str,
        get_time: &F,
    ) -> Result<Option<(NanoTime, T)>> {
        let earliest = self
            .heads
            .iter()
            .enumerate()
            .filter_map(|(i, head)| head.as_ref().map(|(time, _)| (*time, i)))
            .min();
        let Some((_, i)) = earliest else {
            return Ok(None);
        };
        let head = self.heads[i].take();
        self.advance(i, adapter, get_time)?;
        Ok(head)
    }
}

/// Lazy, bounded replay of `groups` as one stream: each record is emitted on
/// the graph clock at `get_time(&record)`, records sharing a timestamp riding
/// one burst.
///
/// Groups (see [`input_files`]) are replayed one after another; the files
/// within a group are merged by `get_time`, ties going to the file that sorts
/// first, so a partition holding several files reads back in time order.
///
/// `open` turns a path into an iterator of record batches and carries its own
/// error context. The first group is opened here, at wiring (fail-fast); the
/// rest are opened on the producer task as the replay reaches them, one group
/// at a time. Each file holds one deserialized batch at a time, so the working
/// set is one batch per file in the open group plus whatever the format's
/// readers hold — paced against the graph by `buffer_size` (see
/// [`produce_async`]). A later open failure, a batch that fails to decode, or
/// one that does not deserialize into `T` aborts the run mid-stream with
/// context naming `adapter` and the file.
///
/// Panics if `groups` is empty; [`input_files`] never returns an empty list.
pub(crate) fn replay_batch_files<T, F, O, R>(
    g: &GraphBuilder,
    adapter: &'static str,
    groups: Vec<Vec<PathBuf>>,
    open: O,
    get_time: F,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<T>>>
where
    T: Clone + Default + DeserializeOwned + Send + 'static,
    F: Fn(&T) -> NanoTime + Send + 'static,
    O: Fn(&Path) -> Result<R> + Send + 'static,
    R: Iterator<Item = std::result::Result<RecordBatch, ArrowError>> + Send + 'static,
{
    let mut groups = groups.into_iter();
    let first = groups
        .next()
        .expect("invariant: input_files never returns an empty list");
    let first = Merge::open(first, &open)?;
    produce_async(
        g,
        move |_p: RunParams| async move {
            Ok(async_stream::stream! {
                let mut current = Some(first);
                while let Some(mut merge) = current.take() {
                    if let Err(e) = merge.prime(adapter, &get_time) {
                        yield Err(e);
                        return;
                    }
                    loop {
                        match merge.next(adapter, &get_time) {
                            Ok(Some(row)) => yield Ok(row),
                            Ok(None) => break,
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        }
                    }
                    if let Some(next) = groups.next() {
                        match Merge::open(next, &open) {
                            Ok(merge) => current = Some(merge),
                            Err(e) => {
                                yield Err(e);
                                return;
                            }
                        }
                    }
                }
            })
        },
        buffer_size,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-04T09:30:15Z — every field distinct from its position, and the
    /// day and hour need zero-padding.
    fn t() -> NanoTime {
        NanoTime::new(1_791_106_215_000_000_000)
    }

    fn dir(time: NanoTime, partition: TimePartition) -> PathBuf {
        PartitionKey::of(time, partition).relative_dir()
    }

    #[test]
    fn epoch_is_the_first_hour_of_1970() {
        assert_eq!(
            dir(NanoTime::ZERO, TimePartition::Hour),
            PathBuf::from("year=1970/month=01/day=01/hour=00")
        );
    }

    #[test]
    fn partition_dirs_nest_and_zero_pad() {
        assert_eq!(dir(t(), TimePartition::Year), PathBuf::from("year=2026"));
        assert_eq!(
            dir(t(), TimePartition::Month),
            PathBuf::from("year=2026/month=10")
        );
        assert_eq!(
            dir(t(), TimePartition::Day),
            PathBuf::from("year=2026/month=10/day=04")
        );
        assert_eq!(
            dir(t(), TimePartition::Hour),
            PathBuf::from("year=2026/month=10/day=04/hour=09")
        );
    }

    /// Two times in one partition share a key; the first instant outside it
    /// does not — the comparison the sink's file switch rides on.
    #[test]
    fn partition_keys_compare_by_granularity() {
        use std::time::Duration;
        let key = PartitionKey::of;
        let same_hour = t() + Duration::from_secs(29 * 60 + 44); // 09:59:59
        let next_hour = t() + Duration::from_secs(29 * 60 + 45); // 10:00:00
        assert_eq!(
            key(t(), TimePartition::Hour),
            key(same_hour, TimePartition::Hour)
        );
        assert_ne!(
            key(t(), TimePartition::Hour),
            key(next_hour, TimePartition::Hour)
        );
        // Coarser granularities ignore the finer fields.
        assert_eq!(
            key(t(), TimePartition::Day),
            key(next_hour, TimePartition::Day)
        );
        let next_year = NanoTime::new(1_798_761_600_000_000_000); // 2027-01-01T00:00:00Z
        assert_eq!(
            key(t(), TimePartition::Year),
            key(
                NanoTime::new(1_798_761_599_999_999_999),
                TimePartition::Year
            )
        );
        assert_ne!(
            key(t(), TimePartition::Year),
            key(next_year, TimePartition::Year)
        );
    }
}
