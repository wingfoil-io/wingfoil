//! Parquet adapter: a serde-typed, lazy, bounded replay **source** and a
//! row-grouped, compressed file **sink** with Hive-style time partitioning,
//! both over the `arrow` adapter's columnar core. Wingfoil-only — there is no
//! legacy adapter to port tests from — so these pin the adapter's own
//! contract: burst grouping and tick times on replay, wiring-time versus
//! mid-stream errors, the written schema, that the sink streams into row
//! groups and survives an aborted run, the partitioned layout round-tripping
//! through a directory read, each codec, and compaction from an Arrow IPC
//! capture.
//!
//! Historical tests run `RunMode::HistoricalFrom(NanoTime::ZERO)` and assert
//! values *and* tick times. The one realtime test asserts what was written and
//! that its times are sane, not the times themselves.

#![cfg(feature = "parquet")]

use std::cell::RefCell;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arrow_array::RecordBatch;
use arrow_schema::{DataType, FieldRef, Schema, SchemaRef, TimeUnit};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::Compression;
use parquet::file::metadata::ParquetMetaData;
use serde::{Deserialize, Serialize};
use serde_arrow::schema::{SchemaLike, TracingOptions};
use wingfoil::adapters::arrow::{ArrowSinkOps, arrow_read};
use wingfoil::adapters::parquet::{
    ParquetCompression, ParquetReadOptions, ParquetSinkOps, ParquetWriteOptions, TimePartition,
    parquet_read, parquet_read_with_options,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Quote {
    ts: u64,
    px: f64,
    qty: i64,
}

fn q(ts: u64, px: f64, qty: i64) -> Quote {
    Quote { ts, px, qty }
}

fn quote_time(r: &Quote) -> NanoTime {
    NanoTime::new(r.ts)
}

/// `Quote` plus a column `Quote` does not name.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WideQuote {
    ts: u64,
    venue: String,
    px: f64,
    qty: i64,
}

/// `Quote`'s shape with `px` as a string — it cannot deserialize into `Quote`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct BadQuote {
    ts: u64,
    px: String,
    qty: i64,
}

/// A row as the sink writes it: the leading `time` column, then `Quote`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Stamped {
    time: i64,
    ts: u64,
    px: f64,
    qty: i64,
}

impl Stamped {
    fn quote(&self) -> Quote {
        q(self.ts, self.px, self.qty)
    }
}

fn stamped_time(s: &Stamped) -> NanoTime {
    NanoTime::new(s.time as u64)
}

/// A unique temp path (pid + process-wide counter) — a file or a directory.
fn tmp(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("wf_parquet_{}_{n}_{name}", std::process::id()))
}

fn fields<T: for<'de> Deserialize<'de>>() -> Vec<FieldRef> {
    Vec::<FieldRef>::from_type::<T>(TracingOptions::default()).unwrap()
}

/// Write `rows` straight through the `parquet` crate (no adapter involved),
/// `chunk` rows per record batch and per row group.
fn write_fixture<T: Serialize + for<'de> Deserialize<'de>>(path: &Path, rows: &[T], chunk: usize) {
    let fields = fields::<T>();
    let schema = std::sync::Arc::new(Schema::new(fields.clone()));
    let props = parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(chunk))
        .build();
    let mut w = ArrowWriter::try_new(File::create(path).unwrap(), schema, Some(props)).unwrap();
    for part in rows.chunks(chunk) {
        w.write(&serde_arrow::to_record_batch(&fields, &part).unwrap())
            .unwrap();
    }
    w.close().unwrap();
}

/// Read a file back with the `parquet` crate directly: its Arrow schema,
/// footer metadata and batches.
fn read_file(path: &Path) -> (SchemaRef, ParquetMetaData, Vec<RecordBatch>) {
    let builder = ParquetRecordBatchReaderBuilder::try_new(File::open(path).unwrap()).unwrap();
    let schema = builder.schema().clone();
    let metadata = builder.metadata().as_ref().clone();
    let batches = builder
        .build()
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    (schema, metadata, batches)
}

/// Every row of a file the sink wrote with its default time column.
fn read_stamped(path: &Path) -> Vec<Stamped> {
    read_file(path)
        .2
        .iter()
        .flat_map(|b| serde_arrow::from_record_batch::<Vec<Stamped>>(b).unwrap())
        .collect()
}

type Ticks = Vec<(NanoTime, Vec<Quote>)>;

fn flatten(ticks: Vec<(NanoTime, Burst<Quote>)>) -> Ticks {
    ticks
        .into_iter()
        .map(|(t, b)| (t, b.into_iter().collect()))
        .collect()
}

/// Replay `path` historically and return every burst with its tick time.
fn replay(path: &Path, options: ParquetReadOptions) -> Ticks {
    let g = GraphBuilder::new();
    let acc = parquet_read_with_options(&g, path, quote_time, options)
        .unwrap()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    flatten(r.value(&acc))
}

/// Replay a sink-written file or tree at the graph times that wrote it.
fn replay_stamped(path: &Path) -> Ticks {
    let g = GraphBuilder::new();
    let acc = parquet_read(&g, path, stamped_time)
        .unwrap()
        .map(|b: &Burst<Stamped>| b.iter().map(Stamped::quote).collect::<Burst<Quote>>())
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    flatten(r.value(&acc))
}

/// `Stream` is not `Debug`, so `expect_err` does not compile on a factory's
/// result.
fn wiring_error<T>(result: anyhow::Result<T>, expectation: &str) -> String {
    match result {
        Ok(_) => panic!("expected a wiring error: {expectation}"),
        Err(e) => format!("{e:#}"),
    }
}

/// Relative paths of every file under `root`, sorted.
fn tree(root: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let rel = path.strip_prefix(root).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

#[test]
fn read_emits_all_rows_each_a_single_burst() {
    let path = tmp("all_rows.parquet");
    let rows: Vec<Quote> = (1..=6).map(|i| q(1000 + i, i as f64, i as i64)).collect();
    write_fixture(&path, &rows, 4); // two row groups: the replay crosses a boundary

    let ticks = replay(&path, ParquetReadOptions::default());
    let expected: Ticks = rows
        .iter()
        .map(|r| (NanoTime::new(r.ts), vec![r.clone()]))
        .collect();
    assert_eq!(ticks, expected);
}

#[test]
fn same_timestamp_rows_ride_one_burst() {
    let path = tmp("same_time.parquet");
    let rows = vec![
        q(1001, 1.0, 1),
        q(1002, 2.0, 2),
        q(1003, 3.0, 3),
        q(1003, 3.5, 4),
        q(1004, 4.0, 5),
    ];
    write_fixture(&path, &rows, 3); // the 1003 pair straddles two row groups

    let options = ParquetReadOptions {
        batch_size: 1, // and every batch boundary
        ..Default::default()
    };
    assert_eq!(
        replay(&path, options),
        vec![
            (NanoTime::new(1001), vec![rows[0].clone()]),
            (NanoTime::new(1002), vec![rows[1].clone()]),
            (NanoTime::new(1003), vec![rows[2].clone(), rows[3].clone()]),
            (NanoTime::new(1004), vec![rows[4].clone()]),
        ]
    );
}

#[test]
fn missing_path_is_a_wiring_error() {
    let g = GraphBuilder::new();
    let err = wiring_error(
        parquet_read(&g, tmp("does_not_exist.parquet"), quote_time),
        "missing file",
    );
    assert!(err.contains("parquet_read: failed to open"), "{err}");
}

#[test]
fn non_parquet_file_is_a_wiring_error() {
    let path = tmp("not_parquet.parquet");
    std::fs::write(&path, "time,px\n1,2.0\n").unwrap();
    let g = GraphBuilder::new();
    let err = wiring_error(parquet_read(&g, &path, quote_time), "not Parquet");
    assert!(
        err.contains("parquet_read: failed to read the Parquet footer of"),
        "{err}"
    );
}

#[test]
fn directory_without_parquet_files_is_a_wiring_error() {
    let dir = tmp("empty_dir");
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    // Present but ignored: only `*.parquet` counts.
    std::fs::write(dir.join("nested").join("_SUCCESS"), "").unwrap();
    std::fs::write(dir.join("data.csv"), "1,2\n").unwrap();
    let g = GraphBuilder::new();
    let err = wiring_error(parquet_read(&g, &dir, quote_time), "no .parquet files");
    assert!(
        err.contains("parquet_read: no .parquet files under"),
        "{err}"
    );
}

#[test]
fn columns_the_record_does_not_name_are_ignored() {
    let path = tmp("wide.parquet");
    let wide = vec![
        WideQuote {
            ts: 10,
            venue: "XLON".into(),
            px: 1.5,
            qty: 3,
        },
        WideQuote {
            ts: 20,
            venue: "XNAS".into(),
            px: 2.5,
            qty: 4,
        },
    ];
    write_fixture(&path, &wide, 16);
    assert_eq!(
        replay(&path, ParquetReadOptions::default()),
        vec![
            (NanoTime::new(10), vec![q(10, 1.5, 3)]),
            (NanoTime::new(20), vec![q(20, 2.5, 4)]),
        ]
    );
}

/// A column whose type cannot deserialize into `T` aborts the run with
/// context, not a panic — and does so mid-stream: the good file ahead of it in
/// the directory has already been delivered.
#[test]
fn undeserializable_column_aborts_the_run_mid_stream() {
    let dir = tmp("bad_types");
    std::fs::create_dir_all(&dir).unwrap();
    write_fixture(&dir.join("a.parquet"), &[q(10, 1.0, 1), q(20, 2.0, 2)], 16);
    // A later directory, so a later group: files sharing a directory are
    // merged, and a merge reads every file's first batch before emitting.
    std::fs::create_dir_all(dir.join("later")).unwrap();
    write_fixture(
        &dir.join("later").join("b.parquet"),
        &[BadQuote {
            ts: 30,
            px: "three".into(),
            qty: 3,
        }],
        16,
    );

    let g = GraphBuilder::new();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = seen.clone();
    let _probe = parquet_read::<Quote, _>(&g, &dir, quote_time)
        .expect("the first file opens fine; the error is in the second")
        .with_time()
        .for_each(move |(t, b)| {
            sink.borrow_mut()
                .push((*t, b.iter().cloned().collect::<Vec<_>>()));
            Ok(())
        });
    let mut r = g.build();
    let err = r
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect_err("the second file's px column is a string");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("parquet_read: failed to deserialize rows from"),
        "{msg}"
    );
    assert!(msg.contains("b.parquet"), "{msg}");
    // Delivery had begun. How much of the good file lands first is the
    // receiver's business (it reads one group past `now` to close a same-time
    // group, so the error can pre-empt the last good group). Pin the prefix.
    let seen = seen.borrow();
    let good = [
        (NanoTime::new(10), vec![q(10, 1.0, 1)]),
        (NanoTime::new(20), vec![q(20, 2.0, 2)]),
    ];
    assert!(!seen.is_empty(), "the first file's rows were delivered");
    assert_eq!(seen[..], good[..seen.len()]);
}

/// The bound changes the producer's pace, never the result — including for a
/// same-time burst larger than the bound, which rides one slot unsplit — and
/// neither does a reader batch size that cuts across timestamp groups.
#[test]
fn bounded_replay_matches_unbounded() {
    let path = tmp("bounded.parquet");
    let mut rows = vec![q(1, 0.0, 0), q(2, 0.0, 1)];
    rows.extend((0..7).map(|i| q(5, 1.0, i))); // a 7-row burst, bound is 2 or 5
    // A few thousand rows in groups of three per timestamp, so 7-row batches
    // never align with them.
    rows.extend((0..3_000).map(|i| q(6 + i / 3, 2.0, i as i64)));
    write_fixture(&path, &rows, 500);

    let unbounded = replay(
        &path,
        ParquetReadOptions {
            buffer_size: None,
            ..Default::default()
        },
    );
    assert_eq!(unbounded.len(), 2 + 1 + 1_000);
    assert_eq!(unbounded[2], (NanoTime::new(5), rows[2..9].to_vec()));
    assert_eq!(unbounded[3], (NanoTime::new(6), rows[9..12].to_vec()));
    for batch_size in [1024, 7] {
        for buffer_size in [None, Some(2), Some(5)] {
            assert_eq!(
                replay(
                    &path,
                    ParquetReadOptions {
                        batch_size,
                        buffer_size
                    }
                ),
                unbounded,
                "batch_size {batch_size}, buffer_size {buffer_size:?}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// read → transform → write → read back: values, tick times, and the written
/// schema — a leading `time: Timestamp(ns)` column, then `Quote`'s fields.
#[test]
fn round_trip_preserves_values_times_and_schema() {
    let input = tmp("rt_in.parquet");
    let output = tmp("rt_out.parquet");
    let rows = vec![
        q(100, 10.0, 1),
        q(200, 11.5, 2),
        q(200, 11.75, 3),
        q(300, 9.75, 4),
    ];
    write_fixture(&input, &rows, 16);

    let g = GraphBuilder::new();
    let bumped = parquet_read(&g, &input, quote_time).unwrap().map(|b| {
        b.iter()
            .map(|r| q(r.ts, r.px + 1.0, r.qty))
            .collect::<Burst<Quote>>()
    });
    let _sink = bumped.parquet_write(&output).unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let (schema, metadata, _) = read_file(&output);
    let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(names, ["time", "ts", "px", "qty"]);
    assert_eq!(
        schema.field(0).data_type(),
        &DataType::Timestamp(TimeUnit::Nanosecond, None)
    );
    assert_eq!(metadata.num_row_groups(), 1);
    assert_eq!(
        read_stamped(&output),
        rows.iter()
            .map(|r| Stamped {
                time: r.ts as i64,
                ts: r.ts,
                px: r.px + 1.0,
                qty: r.qty
            })
            .collect::<Vec<_>>()
    );

    let expected: Ticks = vec![
        (NanoTime::new(100), vec![q(100, 11.0, 1)]),
        (NanoTime::new(200), vec![q(200, 12.5, 2), q(200, 12.75, 3)]),
        (NanoTime::new(300), vec![q(300, 10.75, 4)]),
    ];
    assert_eq!(replay(&output, ParquetReadOptions::default()), expected);
}

#[test]
fn time_column_none_writes_record_columns_only() {
    let input = tmp("notime_in.parquet");
    let output = tmp("notime_out.parquet");
    write_fixture(&input, &[q(1, 1.0, 1), q(2, 2.0, 2)], 16);

    let g = GraphBuilder::new();
    let options = ParquetWriteOptions {
        time_column: None,
        ..Default::default()
    };
    let _sink = parquet_read(&g, &input, quote_time)
        .unwrap()
        .parquet_write_with_options(&output, options)
        .unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let (schema, _, _) = read_file(&output);
    assert_eq!(schema.fields(), Schema::new(fields::<Quote>()).fields());
    assert_eq!(
        replay(&output, ParquetReadOptions::default()),
        vec![
            (NanoTime::new(1), vec![q(1, 1.0, 1)]),
            (NanoTime::new(2), vec![q(2, 2.0, 2)]),
        ]
    );
}

/// A plain `Stream<T>` sinks by wrapping each value in a one-element burst.
#[test]
fn single_value_stream_sink() {
    let output = tmp("single.parquet");
    let g = GraphBuilder::new();
    let quotes = g
        .ticker(Duration::from_nanos(10))
        .count()
        .map(|&n| q(n * 10, n as f64 / 2.0, n as i64));
    let _sink = quotes.parquet_write(&output).unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(3))
        .unwrap();

    assert_eq!(
        read_stamped(&output),
        vec![
            Stamped {
                time: 0,
                ts: 10,
                px: 0.5,
                qty: 1
            },
            Stamped {
                time: 10,
                ts: 20,
                px: 1.0,
                qty: 2
            },
            Stamped {
                time: 20,
                ts: 30,
                px: 1.5,
                qty: 3
            },
        ]
    );
}

#[test]
fn untraceable_record_is_a_wiring_error() {
    let g = GraphBuilder::new();
    let prices = g.ticker(Duration::from_nanos(1)).count().map(|&n| n as f64);
    let err = wiring_error(
        prices.parquet_write(tmp("f64.parquet")),
        "f64 is not a struct",
    );
    assert!(
        err.contains("parquet_write: cannot trace an Arrow schema for `f64`"),
        "{err}"
    );
}

/// A record field named like the sink's time column would write two columns
/// of one name; it is refused at wiring instead.
#[test]
fn record_field_named_like_the_time_column_is_a_wiring_error() {
    let g = GraphBuilder::new();
    let stamped = g.ticker(Duration::from_nanos(1)).count().map(|&n| Stamped {
        time: n as i64,
        ..Default::default()
    });
    let err = wiring_error(
        stamped.parquet_write(tmp("dup_time.parquet")),
        "`time` collides",
    );
    assert!(
        err.contains("parquet_write:") && err.contains("has a field named `time`"),
        "{err}"
    );
    let options = ParquetWriteOptions {
        time_column: Some("graph_time".into()),
        ..Default::default()
    };
    assert!(
        stamped
            .parquet_write_with_options(tmp("renamed_time.parquet"), options)
            .is_ok()
    );
}

/// The sink streams into row groups rather than holding the run: 10 000 rows
/// with `row_group_size: 1000` land as ten full groups, every row is there,
/// and — the streaming half — the file on disk grows while the run is still
/// going, because each full group is encoded and written as it fills.
#[test]
fn row_groups_prove_streaming() {
    const N: u64 = 10_000;
    let path = tmp("row_groups.parquet");
    let g = GraphBuilder::new();
    let options = ParquetWriteOptions {
        row_group_size: 1_000,
        ..Default::default()
    };
    let ticks = g.ticker(Duration::from_nanos(1)).count();
    let _sink = ticks
        .map(|&n| q(n, n as f64, -(n as i64)))
        .parquet_write_with_options(&path, options)
        .unwrap();
    // File size on disk at a few ticks, sampled while the writer is open.
    let sizes = Rc::new(RefCell::new(Vec::new()));
    let probe_sizes = sizes.clone();
    let probe_path = path.clone();
    let _probe = ticks.for_each(move |n| {
        if [1, 2_500, 5_000, 7_500].contains(n) {
            let len = std::fs::metadata(&probe_path).map_or(0, |m| m.len());
            probe_sizes.borrow_mut().push(len);
        }
        Ok(())
    });
    g.build()
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(N as u32),
        )
        .unwrap();

    let (_, metadata, batches) = read_file(&path);
    assert_eq!(metadata.num_row_groups(), 10);
    assert!(
        metadata
            .row_groups()
            .iter()
            .all(|rg| rg.num_rows() == 1_000)
    );
    assert_eq!(
        batches.iter().map(|b| b.num_rows()).sum::<usize>(),
        N as usize
    );
    let rows = read_stamped(&path);
    assert_eq!(rows.first().map(|r| r.ts), Some(1));
    assert_eq!(rows.last().map(|r| r.ts), Some(N));

    let sizes = sizes.take();
    assert_eq!(sizes.len(), 4);
    assert!(
        sizes.windows(2).all(|w| w[1] > w[0]),
        "row groups reach the file mid-run: {sizes:?}"
    );
    let final_len = std::fs::metadata(&path).unwrap().len();
    assert!(sizes[3] < final_len, "{sizes:?} < {final_len}");
}

/// A run aborted by a downstream error still leaves a readable file holding the
/// rows pushed before the abort: they were still pending when the run aborted,
/// so finding them on disk proves the `finally` wrote them and the footer.
#[test]
fn aborted_run_leaves_a_readable_file() {
    let path = tmp("aborted.parquet");
    let g = GraphBuilder::new();
    let ticks = g.ticker(Duration::from_nanos(10)).count();
    let _sink = ticks
        .map(|&n| q(n, n as f64, n as i64))
        .parquet_write(&path)
        .unwrap();
    let _boom = ticks.for_each(|n| {
        if *n == 3 {
            anyhow::bail!("downstream failure on tick 3");
        }
        Ok(())
    });
    let err = g
        .build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
        .expect_err("the probe fails on tick 3");
    assert!(format!("{err:#}").contains("downstream failure on tick 3"));

    let rows = read_stamped(&path);
    // Ticks 1 and 2 certainly; tick 3 too if the sink ran before the failing
    // node in that cycle. Nothing after the abort.
    assert!(rows.len() == 2 || rows.len() == 3, "{rows:?}");
    for (i, row) in rows.iter().enumerate() {
        let n = i as u64 + 1;
        let expected = Stamped {
            time: (n as i64 - 1) * 10,
            ts: n,
            px: n as f64,
            qty: n as i64,
        };
        assert_eq!(*row, expected);
    }
}

/// The sink under `RunMode::RealTime`: the time column is live engine time.
#[test]
fn realtime_write() {
    let path = tmp("realtime.parquet");
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(|&n| q(n, n as f64, n as i64))
        .parquet_write(&path)
        .unwrap();
    g.build().run(RunMode::RealTime, RunFor::Cycles(5)).unwrap();

    let rows = read_stamped(&path);
    assert_eq!(
        rows.iter().map(|r| r.ts).collect::<Vec<_>>(),
        [1, 2, 3, 4, 5]
    );
    assert!(rows.iter().all(|r| r.time > 0), "{rows:?}");
    assert!(rows.windows(2).all(|w| w[0].time <= w[1].time), "{rows:?}");
}

/// Every codec round-trips, and the footer reports it on every column chunk.
#[test]
fn each_compression_round_trips_and_is_recorded() {
    let rows: Vec<Quote> = (1..=50).map(|i| q(i, i as f64 * 0.5, i as i64)).collect();
    for (compression, codec) in [
        (ParquetCompression::Uncompressed, Compression::UNCOMPRESSED),
        (ParquetCompression::Snappy, Compression::SNAPPY),
        (
            ParquetCompression::Zstd,
            Compression::ZSTD(Default::default()),
        ),
    ] {
        let input = tmp("codec_in.parquet");
        let output = tmp("codec_out.parquet");
        write_fixture(&input, &rows, 64);
        let g = GraphBuilder::new();
        let options = ParquetWriteOptions {
            compression,
            ..Default::default()
        };
        let _sink = parquet_read(&g, &input, quote_time)
            .unwrap()
            .parquet_write_with_options(&output, options)
            .unwrap();
        g.build()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();

        let (_, metadata, _) = read_file(&output);
        for rg in metadata.row_groups() {
            for column in rg.columns() {
                // The footer records the codec, not its level.
                assert_eq!(
                    std::mem::discriminant(&column.compression()),
                    std::mem::discriminant(&codec),
                    "{compression:?}: {}",
                    column.column_path()
                );
            }
        }
        assert_eq!(
            replay(&output, ParquetReadOptions::default()),
            replay(&input, ParquetReadOptions::default()),
            "{compression:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Hive partitioning
// ---------------------------------------------------------------------------

const HOUR: u64 = 3_600 * 1_000_000_000;
const DAY: u64 = 24 * HOUR;
/// 2026-10-02T00:00:00Z.
const OCT_2: u64 = 1_790_899_200_000_000_000;

/// Rows across three UTC days, one same-time pair among them.
fn three_days() -> Vec<Quote> {
    vec![
        q(OCT_2 + 9 * HOUR, 1.0, 1),
        q(OCT_2 + 23 * HOUR, 2.0, 2),
        q(OCT_2 + DAY + HOUR, 3.0, 3),
        q(OCT_2 + DAY + HOUR, 3.5, 4),
        q(OCT_2 + 2 * DAY, 4.0, 5),
        q(OCT_2 + 2 * DAY + 5 * HOUR, 5.0, 6),
    ]
}

/// Replay `rows` from a fixture into a partitioned sink under a fresh root.
fn write_partitioned(
    rows: &[Quote],
    partition: TimePartition,
    options: ParquetWriteOptions,
) -> PathBuf {
    let input = tmp("part_in.parquet");
    write_fixture(&input, rows, 2);
    let root = tmp("part_root");
    let g = GraphBuilder::new();
    let _sink = parquet_read(&g, &input, quote_time)
        .unwrap()
        .parquet_write_partitioned_with_options(&root, partition, options)
        .unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    root
}

#[test]
fn partitioned_by_day_round_trips_through_the_root() {
    let rows = three_days();
    let root = write_partitioned(&rows, TimePartition::Day, ParquetWriteOptions::default());

    assert_eq!(
        tree(&root),
        [
            "year=2026/month=10/day=02/data.parquet",
            "year=2026/month=10/day=03/data.parquet",
            "year=2026/month=10/day=04/data.parquet",
        ]
    );
    let day = |d: &str| -> Vec<Quote> {
        read_stamped(&root.join(format!("year=2026/month=10/day={d}/data.parquet")))
            .iter()
            .map(Stamped::quote)
            .collect()
    };
    assert_eq!(day("02"), rows[0..2]);
    assert_eq!(day("03"), rows[2..4]);
    assert_eq!(day("04"), rows[4..6]);

    let input = tmp("part_direct.parquet");
    write_fixture(&input, &rows, 16);
    let original = replay(&input, ParquetReadOptions::default());
    assert_eq!(original.len(), 5);
    assert_eq!(replay(&root, ParquetReadOptions::default()), original);
    assert_eq!(replay_stamped(&root), original);
}

#[test]
fn partitioned_by_hour_and_year() {
    let rows = three_days();
    let hour = write_partitioned(&rows, TimePartition::Hour, ParquetWriteOptions::default());
    assert_eq!(
        tree(&hour),
        [
            "year=2026/month=10/day=02/hour=09/data.parquet",
            "year=2026/month=10/day=02/hour=23/data.parquet",
            "year=2026/month=10/day=03/hour=01/data.parquet",
            "year=2026/month=10/day=04/hour=00/data.parquet",
            "year=2026/month=10/day=04/hour=05/data.parquet",
        ]
    );
    let year = write_partitioned(&rows, TimePartition::Year, ParquetWriteOptions::default());
    assert_eq!(tree(&year), ["year=2026/data.parquet"]);
    assert_eq!(
        read_stamped(&year.join("year=2026/data.parquet")).len(),
        rows.len()
    );
}

#[test]
fn partitioned_file_name_override() {
    let options = ParquetWriteOptions {
        file_name: "archive.parquet".into(),
        ..Default::default()
    };
    let root = write_partitioned(&three_days(), TimePartition::Month, options);
    assert_eq!(tree(&root), ["year=2026/month=10/archive.parquet"]);
}

/// Compaction is a graph: an hourly Arrow IPC capture read back and written as
/// a daily Parquet tree reproduces the stream that was captured.
#[test]
fn compaction_from_arrow_capture() {
    let rows = three_days();
    let input = tmp("compact_in.parquet");
    write_fixture(&input, &rows, 16);
    let original = replay(&input, ParquetReadOptions::default());
    // Captured a minute after each record's own `ts`, so the graph-time
    // column and `ts` differ and the test can tell which drives a replay.
    const LAG: u64 = 60_000_000_000;
    let captured: Ticks = original
        .iter()
        .map(|(t, b)| (*t + Duration::from_nanos(LAG), b.clone()))
        .collect();

    // Capture: hourly Arrow IPC, flushed every tick.
    let capture = tmp("compact_arrow");
    let g = GraphBuilder::new();
    let _sink = parquet_read(&g, &input, |r: &Quote| NanoTime::new(r.ts + LAG))
        .unwrap()
        .arrow_write_partitioned(&capture, TimePartition::Hour)
        .unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    assert_eq!(tree(&capture).len(), 5);

    // Compact: the capture's own `time` column drives the replay, so the
    // archive is written at the captured graph times.
    let archive = tmp("compact_parquet");
    let g = GraphBuilder::new();
    let _sink = arrow_read(&g, &capture, stamped_time)
        .unwrap()
        .map(|b: &Burst<Stamped>| b.iter().map(Stamped::quote).collect::<Burst<Quote>>())
        .parquet_write_partitioned(&archive, TimePartition::Day)
        .unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    assert_eq!(
        tree(&archive),
        [
            "year=2026/month=10/day=02/data.parquet",
            "year=2026/month=10/day=03/data.parquet",
            "year=2026/month=10/day=04/data.parquet",
        ]
    );
    // Replayed by its graph-time column, the archive is the captured stream;
    // replayed by the records' own `ts`, it is the original one.
    assert_eq!(replay_stamped(&archive), captured);
    assert_eq!(replay(&archive, ParquetReadOptions::default()), original);
    assert_ne!(captured, original);
}

/// Interop pin: a written tree is readable by other tools with Hive
/// partitioning on. Always reads every file with the `parquet` crate directly,
/// bypassing the adapter; additionally reads the tree with `pyarrow` when
/// `python3` has it, and skips that half with an `eprintln!` otherwise.
#[test]
fn partitioned_tree_is_readable_outside_the_adapter() {
    let rows = three_days();
    let root = write_partitioned(&rows, TimePartition::Day, ParquetWriteOptions::default());
    let first = (OCT_2 + 9 * HOUR) as i64;
    let last = (OCT_2 + 2 * DAY + 5 * HOUR) as i64;

    // Pure Rust, no adapter: every file's rows in path order.
    let direct: Vec<Stamped> = tree(&root)
        .iter()
        .flat_map(|rel| read_stamped(&root.join(rel)))
        .collect();
    assert_eq!(direct.len(), rows.len());
    assert_eq!(direct.first().map(|r| r.time), Some(first));
    assert_eq!(direct.last().map(|r| r.time), Some(last));

    let script = r#"
import sys
import pyarrow.dataset as ds
t = ds.dataset(sys.argv[1], format="parquet", partitioning="hive").to_table()
assert {"year", "month", "day"} <= set(t.column_names), t.column_names
times = t.sort_by("time").column("time").cast("int64").to_pylist()
print(t.num_rows, times[0], times[-1])
"#;
    let output = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(&root)
        .output();
    match output {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert_eq!(
                stdout.trim(),
                format!("{} {first} {last}", rows.len()),
                "pyarrow read"
            );
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains("No module named"),
                "pyarrow failed to read the tree: {stderr}"
            );
            eprintln!("skipping the pyarrow read: pyarrow is not installed");
        }
        Err(e) => eprintln!("skipping the pyarrow read: no python3 ({e})"),
    }
}
