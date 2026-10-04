//! Arrow IPC adapter: a serde-typed, lazy, bounded replay **source** and a
//! flush-per-tick, crash-tolerant file **sink** with Hive-style time
//! partitioning. Wingfoil-only — there is no legacy adapter to port tests from —
//! so these pin the adapter's own contract: burst grouping and tick times on
//! replay, wiring-time versus mid-stream errors, the written schema, that the
//! sink streams (readable mid-run, per tick) and survives an aborted run, and
//! the partitioned layout round-tripping through a directory read.
//!
//! Historical tests run `RunMode::HistoricalFrom(NanoTime::ZERO)` and assert
//! values *and* tick times. The one realtime test asserts what was written and
//! that its times are sane, not the times themselves.

#![cfg(feature = "arrow")]

use std::cell::RefCell;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use arrow_array::RecordBatch;
use arrow_ipc::reader::StreamReader;
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{DataType, FieldRef, Schema, SchemaRef, TimeUnit};
use serde::{Deserialize, Serialize};
use serde_arrow::schema::{SchemaLike, TracingOptions};
use wingfoil::adapters::arrow::{
    ArrowReadOptions, ArrowSinkOps, ArrowWriteOptions, TimePartition, arrow_read,
    arrow_read_with_options,
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

/// A unique temp path (pid + process-wide counter) — a file or a directory.
fn tmp(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("wf_arrow_{}_{n}_{name}", std::process::id()))
}

fn fields<T: for<'de> Deserialize<'de>>() -> Vec<FieldRef> {
    Vec::<FieldRef>::from_type::<T>(TracingOptions::default()).unwrap()
}

/// Write `rows` straight through `arrow_ipc` (no adapter involved), `chunk`
/// rows per record batch.
fn write_fixture<T: Serialize + for<'de> Deserialize<'de>>(path: &Path, rows: &[T], chunk: usize) {
    let fields = fields::<T>();
    let schema = Schema::new(fields.clone());
    let mut w = StreamWriter::try_new(File::create(path).unwrap(), &schema).unwrap();
    for part in rows.chunks(chunk) {
        w.write(&serde_arrow::to_record_batch(&fields, &part).unwrap())
            .unwrap();
    }
    w.finish().unwrap();
}

/// Read a file back with `arrow_ipc` directly: its schema and batches.
fn read_batches(path: &Path) -> (SchemaRef, Vec<RecordBatch>) {
    let reader = StreamReader::try_new(File::open(path).unwrap(), None).unwrap();
    let schema = reader.schema();
    let batches = reader.collect::<Result<Vec<_>, _>>().unwrap();
    (schema, batches)
}

/// Every row of a file the sink wrote with its default time column.
fn read_stamped(path: &Path) -> Vec<Stamped> {
    read_batches(path)
        .1
        .iter()
        .flat_map(|b| serde_arrow::from_record_batch::<Vec<Stamped>>(b).unwrap())
        .collect()
}

/// Rows readable *right now*, through a reader that tolerates a stream the
/// writer has not finished (no end-of-stream marker yet).
fn rows_now(path: &Path) -> usize {
    let reader = StreamReader::try_new(File::open(path).unwrap(), None).unwrap();
    reader.map(|b| b.unwrap().num_rows()).sum()
}

type Ticks = Vec<(NanoTime, Vec<Quote>)>;

fn flatten(ticks: Vec<(NanoTime, Burst<Quote>)>) -> Ticks {
    ticks
        .into_iter()
        .map(|(t, b)| (t, b.into_iter().collect()))
        .collect()
}

/// Replay `path` historically and return every burst with its tick time.
fn replay(path: &Path, options: ArrowReadOptions) -> Ticks {
    let g = GraphBuilder::new();
    let acc = arrow_read_with_options(&g, path, quote_time, options)
        .unwrap()
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
    let path = tmp("all_rows.arrows");
    let rows: Vec<Quote> = (1..=6).map(|i| q(1000 + i, i as f64, i as i64)).collect();
    write_fixture(&path, &rows, 4); // two batches: the replay crosses a boundary

    let ticks = replay(&path, ArrowReadOptions::default());
    let expected: Ticks = rows
        .iter()
        .map(|r| (NanoTime::new(r.ts), vec![r.clone()]))
        .collect();
    assert_eq!(ticks, expected);
}

#[test]
fn same_timestamp_rows_ride_one_burst() {
    let path = tmp("same_time.arrows");
    let rows = vec![
        q(1001, 1.0, 1),
        q(1002, 2.0, 2),
        q(1003, 3.0, 3),
        q(1003, 3.5, 4),
        q(1004, 4.0, 5),
    ];
    write_fixture(&path, &rows, 1); // the 1003 pair straddles two batches

    let ticks = replay(&path, ArrowReadOptions::default());
    assert_eq!(
        ticks,
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
        arrow_read(&g, tmp("does_not_exist.arrows"), quote_time),
        "missing file",
    );
    assert!(err.contains("arrow_read: failed to open"), "{err}");
}

#[test]
fn non_ipc_file_is_a_wiring_error() {
    let path = tmp("not_ipc.arrows");
    std::fs::write(&path, "time,px\n1,2.0\n").unwrap();
    let g = GraphBuilder::new();
    let err = wiring_error(arrow_read(&g, &path, quote_time), "not an IPC stream");
    assert!(
        err.contains("arrow_read: failed to read the Arrow IPC stream header"),
        "{err}"
    );
}

#[test]
fn directory_without_arrows_files_is_a_wiring_error() {
    let dir = tmp("empty_dir");
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    // Present but ignored: only `*.arrows` counts.
    std::fs::write(dir.join("nested").join("_SUCCESS"), "").unwrap();
    std::fs::write(dir.join("data.csv"), "1,2\n").unwrap();
    let g = GraphBuilder::new();
    let err = wiring_error(arrow_read(&g, &dir, quote_time), "no .arrows files");
    assert!(err.contains("arrow_read: no .arrows files under"), "{err}");
}

#[test]
fn columns_the_record_does_not_name_are_ignored() {
    let path = tmp("wide.arrows");
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
        replay(&path, ArrowReadOptions::default()),
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
    write_fixture(&dir.join("a.arrows"), &[q(10, 1.0, 1), q(20, 2.0, 2)], 16);
    // A later directory, so a later group: files sharing a directory are
    // merged, and a merge reads every file's first batch before emitting.
    std::fs::create_dir_all(dir.join("later")).unwrap();
    write_fixture(
        &dir.join("later").join("b.arrows"),
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
    let _probe = arrow_read::<Quote, _>(&g, &dir, quote_time)
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
        msg.contains("arrow_read: failed to deserialize rows from"),
        "{msg}"
    );
    assert!(msg.contains("b.arrows"), "{msg}");
    // Delivery had begun. How much of the good file lands first is the
    // receiver's business, not the adapter's: it reads one group past `now`
    // to close a same-time group, so the error can pre-empt the last good
    // group (today it does: only t=10 is seen). Pin the prefix, not its length.
    let seen = seen.borrow();
    let good = [
        (NanoTime::new(10), vec![q(10, 1.0, 1)]),
        (NanoTime::new(20), vec![q(20, 2.0, 2)]),
    ];
    assert!(!seen.is_empty(), "the first file's rows were delivered");
    assert_eq!(seen[..], good[..seen.len()]);
}

/// The bound changes the producer's pace, never the result — including for a
/// same-time burst larger than the bound, which rides one slot unsplit.
#[test]
fn bounded_replay_matches_unbounded() {
    let path = tmp("bounded.arrows");
    let mut rows = vec![q(1, 0.0, 0), q(2, 0.0, 1)];
    rows.extend((0..7).map(|i| q(5, 1.0, i))); // a 7-row burst, bound is 2 or 5
    rows.extend((6..20).map(|t| q(t, 2.0, t as i64)));
    write_fixture(&path, &rows, 3);

    let unbounded = replay(&path, ArrowReadOptions { buffer_size: None });
    assert_eq!(unbounded.len(), 2 + 1 + 14);
    assert_eq!(unbounded[2], (NanoTime::new(5), rows[2..9].to_vec()));
    for bound in [Some(2), Some(5)] {
        assert_eq!(
            replay(&path, ArrowReadOptions { buffer_size: bound }),
            unbounded,
            "buffer_size {bound:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// read → transform → write → read back: values, tick times, and the written
/// schema — a leading `time: Timestamp(ns)` column, then `Quote`'s fields.
#[test]
fn round_trip_preserves_values_times_and_schema() {
    let input = tmp("rt_in.arrows");
    let output = tmp("rt_out.arrows");
    let rows = vec![
        q(100, 10.0, 1),
        q(200, 11.5, 2),
        q(200, 11.75, 3),
        q(300, 9.75, 4),
    ];
    write_fixture(&input, &rows, 16);

    let g = GraphBuilder::new();
    let bumped = arrow_read(&g, &input, quote_time).unwrap().map(|b| {
        b.iter()
            .map(|r| q(r.ts, r.px + 1.0, r.qty))
            .collect::<Burst<Quote>>()
    });
    let _sink = bumped.arrow_write(&output).unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let (schema, batches) = read_batches(&output);
    let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
    assert_eq!(names, ["time", "ts", "px", "qty"]);
    assert_eq!(
        schema.field(0).data_type(),
        &DataType::Timestamp(TimeUnit::Nanosecond, None)
    );
    // One batch per tick (flush_every_tick), the same-time pair in one.
    assert_eq!(
        batches.iter().map(|b| b.num_rows()).collect::<Vec<_>>(),
        [1, 2, 1]
    );
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
    assert_eq!(replay(&output, ArrowReadOptions::default()), expected);
}

/// The sink's `time` column reads back as `i64` nanoseconds, so a record type
/// that names it replays a capture at the graph times that wrote it — here
/// times that differ from the records' own `ts`.
#[test]
fn time_column_reads_back_as_i64_and_drives_replay() {
    let path = tmp("captured.arrows");
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_nanos(100))
        .count()
        .map(|&n| q(n, n as f64, n as i64))
        .arrow_write(&path)
        .unwrap();
    g.build()
        .run(
            RunMode::HistoricalFrom(NanoTime::new(1_000)),
            RunFor::Cycles(3),
        )
        .unwrap();

    let g = GraphBuilder::new();
    let acc = arrow_read(&g, &path, |s: &Stamped| NanoTime::new(s.time as u64))
        .unwrap()
        .with_time()
        .accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    let ticks: Vec<(NanoTime, Vec<Stamped>)> = r
        .value(&acc)
        .into_iter()
        .map(|(t, b)| (t, b.into_iter().collect()))
        .collect();
    let row = |time: i64, n: u64| Stamped {
        time,
        ts: n,
        px: n as f64,
        qty: n as i64,
    };
    assert_eq!(
        ticks,
        vec![
            (NanoTime::new(1_000), vec![row(1_000, 1)]),
            (NanoTime::new(1_100), vec![row(1_100, 2)]),
            (NanoTime::new(1_200), vec![row(1_200, 3)]),
        ]
    );
}

#[test]
fn time_column_none_writes_record_columns_only() {
    let input = tmp("notime_in.arrows");
    let output = tmp("notime_out.arrows");
    write_fixture(&input, &[q(1, 1.0, 1), q(2, 2.0, 2)], 16);

    let g = GraphBuilder::new();
    let options = ArrowWriteOptions {
        time_column: None,
        ..Default::default()
    };
    let _sink = arrow_read(&g, &input, quote_time)
        .unwrap()
        .arrow_write_with_options(&output, options)
        .unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let (schema, _) = read_batches(&output);
    assert_eq!(schema.fields(), Schema::new(fields::<Quote>()).fields());
    assert_eq!(
        replay(&output, ArrowReadOptions::default()),
        vec![
            (NanoTime::new(1), vec![q(1, 1.0, 1)]),
            (NanoTime::new(2), vec![q(2, 2.0, 2)]),
        ]
    );
}

/// A plain `Stream<T>` sinks by wrapping each value in a one-element burst.
#[test]
fn single_value_stream_sink() {
    let output = tmp("single.arrows");
    let g = GraphBuilder::new();
    let quotes = g
        .ticker(Duration::from_nanos(10))
        .count()
        .map(|&n| q(n * 10, n as f64 / 2.0, n as i64));
    let _sink = quotes.arrow_write(&output).unwrap();
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
    let err = wiring_error(prices.arrow_write(tmp("f64.arrows")), "f64 is not a struct");
    assert!(
        err.contains("arrow_write: cannot trace an Arrow schema for `f64`"),
        "{err}"
    );
}

/// Several thousand rows written with a small `batch_size` and no per-tick
/// flush land as many full batches; the replay crosses every boundary and
/// matches across bounds.
#[test]
fn many_batches_round_trip() {
    const N: u64 = 5_000;
    const BATCH: usize = 64;
    let path = tmp("many.arrows");
    let g = GraphBuilder::new();
    let quotes = g
        .ticker(Duration::from_nanos(1))
        .count()
        .map(|&n| q(n, n as f64, -(n as i64)));
    let options = ArrowWriteOptions {
        batch_size: BATCH,
        flush_every_tick: false,
        ..Default::default()
    };
    let _sink = quotes.arrow_write_with_options(&path, options).unwrap();
    g.build()
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(N as u32),
        )
        .unwrap();

    let (_, batches) = read_batches(&path);
    let sizes: Vec<usize> = batches.iter().map(|b| b.num_rows()).collect();
    assert_eq!(sizes.len(), (N as usize).div_ceil(BATCH));
    assert!(sizes[..sizes.len() - 1].iter().all(|&s| s == BATCH));
    assert_eq!(sizes.iter().sum::<usize>(), N as usize);

    let expected: Ticks = (1..=N)
        .map(|n| (NanoTime::new(n), vec![q(n, n as f64, -(n as i64))]))
        .collect();
    for bound in [None, Some(2), Some(5)] {
        assert_eq!(
            replay(&path, ArrowReadOptions { buffer_size: bound }),
            expected,
            "{bound:?}"
        );
    }
}

/// Wire a ticker → quote → sink graph plus a probe, *wired after the sink*,
/// that counts the rows readable from the file on every tick while the writer
/// is still open.
fn probe_while_writing(path: &Path, options: ArrowWriteOptions, cycles: u32) -> Vec<usize> {
    let g = GraphBuilder::new();
    let ticks = g.ticker(Duration::from_nanos(10)).count();
    let _sink = ticks
        .map(|&n| q(n, n as f64, n as i64))
        .arrow_write_with_options(path, options)
        .unwrap();
    let seen = Rc::new(RefCell::new(Vec::new()));
    let probe_seen = seen.clone();
    let probe_path = path.to_path_buf();
    let _probe = ticks.for_each(move |_| {
        probe_seen.borrow_mut().push(rows_now(&probe_path));
        Ok(())
    });
    g.build()
        .run(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(cycles),
        )
        .unwrap();
    seen.take()
}

/// Per-tick flush: the open file is readable mid-run and grows by one batch a
/// tick. The probe runs in the same cycle as the sink, before or after it, so
/// on tick `k` it sees `k - 1` or `k` rows; either way the count climbs every
/// tick. After the run the file holds every row, one batch each.
#[test]
fn sink_streams_one_batch_per_tick() {
    let path = tmp("streams.arrows");
    let seen = probe_while_writing(&path, ArrowWriteOptions::default(), 5);
    assert_eq!(seen.len(), 5);
    for (k, rows) in seen.iter().enumerate() {
        assert!(*rows == k || *rows == k + 1, "tick {}: {rows} rows", k + 1);
    }
    assert!(seen.windows(2).all(|w| w[1] == w[0] + 1), "{seen:?}");

    let (_, batches) = read_batches(&path);
    assert_eq!(
        batches.iter().map(|b| b.num_rows()).collect::<Vec<_>>(),
        [1; 5]
    );
}

/// Without per-tick flush, and a batch that never fills, nothing reaches the
/// file until teardown writes it as one batch.
#[test]
fn sink_without_per_tick_flush_writes_at_teardown() {
    let path = tmp("teardown.arrows");
    let options = ArrowWriteOptions {
        flush_every_tick: false,
        batch_size: 1_000,
        ..Default::default()
    };
    let seen = probe_while_writing(&path, options, 5);
    assert_eq!(seen, [0; 5]);

    let (_, batches) = read_batches(&path);
    assert_eq!(
        batches.iter().map(|b| b.num_rows()).collect::<Vec<_>>(),
        [5]
    );
}

/// A run aborted by a downstream error still leaves a readable file holding the
/// rows written before the abort. Without per-tick flush those rows are still
/// pending when the run aborts, so finding them on disk proves the `finally`
/// ran at teardown and wrote them.
#[test]
fn aborted_run_leaves_a_readable_file() {
    let per_tick = ArrowWriteOptions::default();
    let at_teardown = ArrowWriteOptions {
        flush_every_tick: false,
        ..Default::default()
    };
    for options in [per_tick, at_teardown] {
        let path = tmp("aborted.arrows");
        let g = GraphBuilder::new();
        let ticks = g.ticker(Duration::from_nanos(10)).count();
        let _sink = ticks
            .map(|&n| q(n, n as f64, n as i64))
            .arrow_write_with_options(&path, options.clone())
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
        // Ticks 1 and 2 certainly; tick 3 too if the sink ran before the
        // failing node in that cycle. Nothing after the abort.
        assert!(rows.len() == 2 || rows.len() == 3, "{options:?}: {rows:?}");
        for (i, row) in rows.iter().enumerate() {
            let n = i as u64 + 1;
            let expected = Stamped {
                time: (n as i64 - 1) * 10,
                ts: n,
                px: n as f64,
                qty: n as i64,
            };
            assert_eq!(*row, expected, "{options:?}");
        }
    }
}

/// The sink under `RunMode::RealTime`: the time column is live engine time.
#[test]
fn realtime_write() {
    let path = tmp("realtime.arrows");
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_millis(1))
        .count()
        .map(|&n| q(n, n as f64, n as i64))
        .arrow_write(&path)
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
    options: ArrowWriteOptions,
) -> PathBuf {
    let input = tmp("part_in.arrows");
    write_fixture(&input, rows, 2);
    let root = tmp("part_root");
    let g = GraphBuilder::new();
    let _sink = arrow_read(&g, &input, quote_time)
        .unwrap()
        .arrow_write_partitioned_with_options(&root, partition, options)
        .unwrap();
    g.build()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    root
}

#[test]
fn partitioned_by_day_round_trips_through_the_root() {
    let rows = three_days();
    let root = write_partitioned(&rows, TimePartition::Day, ArrowWriteOptions::default());

    assert_eq!(
        tree(&root),
        [
            "year=2026/month=10/day=02/data.arrows",
            "year=2026/month=10/day=03/data.arrows",
            "year=2026/month=10/day=04/data.arrows",
        ]
    );
    let day = |d: &str| -> Vec<Quote> {
        read_stamped(&root.join(format!("year=2026/month=10/day={d}/data.arrows")))
            .into_iter()
            .map(|s| q(s.ts, s.px, s.qty))
            .collect()
    };
    assert_eq!(day("02"), rows[0..2]);
    assert_eq!(day("03"), rows[2..4]);
    assert_eq!(day("04"), rows[4..6]);

    let input = tmp("part_direct.arrows");
    write_fixture(&input, &rows, 16);
    let original = replay(&input, ArrowReadOptions::default());
    assert_eq!(original.len(), 5);
    assert_eq!(replay(&root, ArrowReadOptions::default()), original);
}

#[test]
fn partitioned_by_hour_and_year() {
    let rows = three_days();
    let hour = write_partitioned(&rows, TimePartition::Hour, ArrowWriteOptions::default());
    assert_eq!(
        tree(&hour),
        [
            "year=2026/month=10/day=02/hour=09/data.arrows",
            "year=2026/month=10/day=02/hour=23/data.arrows",
            "year=2026/month=10/day=03/hour=01/data.arrows",
            "year=2026/month=10/day=04/hour=00/data.arrows",
            "year=2026/month=10/day=04/hour=05/data.arrows",
        ]
    );
    let year = write_partitioned(&rows, TimePartition::Year, ArrowWriteOptions::default());
    assert_eq!(tree(&year), ["year=2026/data.arrows"]);
    assert_eq!(
        read_stamped(&year.join("year=2026/data.arrows")).len(),
        rows.len()
    );
}

#[test]
fn partitioned_file_name_override() {
    let options = ArrowWriteOptions {
        file_name: "capture.arrows".into(),
        ..Default::default()
    };
    let root = write_partitioned(&three_days(), TimePartition::Month, options);
    assert_eq!(tree(&root), ["year=2026/month=10/capture.arrows"]);
}

#[test]
fn partitioned_root_is_created_at_wiring() {
    let root = tmp("part_wiring").join("a").join("b");
    let g = GraphBuilder::new();
    let _sink = g
        .ticker(Duration::from_nanos(1))
        .count()
        .map(|&n| q(n, 0.0, 0))
        .arrow_write_partitioned(&root, TimePartition::Day)
        .unwrap();
    assert!(root.is_dir());
    assert!(tree(&root).is_empty(), "no partition before the first row");
}

/// Two runs into one root under different file names leave two files in each
/// partition, their rows interleaved in time. A root read merges them rather
/// than replaying one file after the other (which would go back in time).
#[test]
fn files_sharing_a_partition_merge_by_time() {
    // Alternating rows go to each run, so within a day `a`'s second row is
    // later than `b`'s first. The same-time pair on the 3rd splits across runs.
    let rows = vec![
        q(OCT_2 + HOUR, 1.0, 1),
        q(OCT_2 + 2 * HOUR, 2.0, 2),
        q(OCT_2 + 3 * HOUR, 3.0, 3),
        q(OCT_2 + 4 * HOUR, 4.0, 4),
        q(OCT_2 + DAY, 5.0, 5),
        q(OCT_2 + DAY, 5.5, 6),
        q(OCT_2 + 2 * DAY, 6.0, 7),
        q(OCT_2 + 2 * DAY + HOUR, 7.0, 8),
    ];
    let root = tmp("part_shared");
    for (name, parity) in [("a.arrows", 0), ("b.arrows", 1)] {
        let mine: Vec<Quote> = rows
            .iter()
            .enumerate()
            .filter(|(i, _)| i % 2 == parity)
            .map(|(_, r)| r.clone())
            .collect();
        let input = tmp("part_shared_in.arrows");
        write_fixture(&input, &mine, 1);
        let g = GraphBuilder::new();
        let options = ArrowWriteOptions {
            file_name: name.into(),
            ..Default::default()
        };
        let _sink = arrow_read(&g, &input, quote_time)
            .unwrap()
            .arrow_write_partitioned_with_options(&root, TimePartition::Day, options)
            .unwrap();
        g.build()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
    }
    assert_eq!(
        tree(&root),
        [
            "year=2026/month=10/day=02/a.arrows",
            "year=2026/month=10/day=02/b.arrows",
            "year=2026/month=10/day=03/a.arrows",
            "year=2026/month=10/day=03/b.arrows",
            "year=2026/month=10/day=04/a.arrows",
            "year=2026/month=10/day=04/b.arrows",
        ]
    );

    let input = tmp("part_shared_direct.arrows");
    write_fixture(&input, &rows, 16);
    let original = replay(&input, ArrowReadOptions::default());
    assert_eq!(replay(&root, ArrowReadOptions::default()), original);
}

/// `Quote` whose `qty` is skipped when negative: tracing sees a required
/// column, so such a record fails to serialize part-way — after `ts`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Flaky {
    ts: u64,
    #[serde(skip_serializing_if = "is_negative")]
    qty: i64,
}

fn is_negative(qty: &i64) -> bool {
    *qty < 0
}

#[derive(Debug, PartialEq, Deserialize)]
struct StampedFlaky {
    time: i64,
    ts: u64,
    qty: i64,
}

/// A record that fails to serialize aborts the run with context, and the rows
/// pushed before it — earlier ticks still pending, and earlier rows of its own
/// burst — are still written at teardown.
#[test]
fn unserializable_record_keeps_earlier_rows() {
    let per_tick = ArrowWriteOptions::default();
    let at_teardown = ArrowWriteOptions {
        flush_every_tick: false,
        ..Default::default()
    };
    for options in [per_tick, at_teardown] {
        let path = tmp("flaky.arrows");
        let g = GraphBuilder::new();
        let _sink = g
            .ticker(Duration::from_nanos(10))
            .count()
            .map(|&n| {
                let qty = if n == 3 { -1 } else { n as i64 };
                vec![Flaky { ts: n, qty: 0 }, Flaky { ts: n, qty }]
                    .into_iter()
                    .collect::<Burst<Flaky>>()
            })
            .arrow_write_with_options(&path, options.clone())
            .unwrap();
        let err = g
            .build()
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(10))
            .expect_err("tick 3's second record cannot serialize");
        let msg = format!("{err:#}");
        assert!(msg.contains("failed to serialize record"), "{msg}");

        let (_, batches) = read_batches(&path);
        let rows: Vec<StampedFlaky> = batches
            .iter()
            .flat_map(|b| serde_arrow::from_record_batch::<Vec<StampedFlaky>>(b).unwrap())
            .collect();
        let row = |n: u64, qty: i64| StampedFlaky {
            time: (n as i64 - 1) * 10,
            ts: n,
            qty,
        };
        assert_eq!(
            rows,
            [row(1, 0), row(1, 1), row(2, 0), row(2, 2), row(3, 0)],
            "{options:?}"
        );
    }
}
