//! polars adapter: a DataFrame / Parquet / Arrow IPC historical replay
//! **source** and a DataFrame-collecting (and Parquet / IPC-writing) **sink**.
//!
//! Wingfoil-only — there is no legacy polars adapter to port parity tests from —
//! so these pin the adapter's own contract: rows replay at their time column's
//! instants with same-instant rows in one burst (values *and* tick times), every
//! supported time-column dtype maps onto the graph clock, a bad time column is a
//! wiring-time `Err`, and a read → transform → write round trip reproduces the
//! frame through both file formats.

#![cfg(feature = "polars")]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use wingfoil::adapters::polars::{
    AnyValue, DataFrame, DataType, PolarsFormat, PolarsRow, PolarsSinkOps, PolarsSinkOptions,
    PolarsSource, Schema, TimeUnit, polars_read,
};
use wingfoil::prelude::*;
use wingfoil::{NanoTime, RunFor, RunMode};

use wingfoil::adapters::polars::polars::prelude::{Column, IntoColumn, NamedFrom, Series};

/// A unique temp path per call (pid + process-wide counter), so parallel tests
/// never collide.
fn tmp_path(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("wf_polars_{}_{}_{name}", std::process::id(), n))
}

/// `time` (i64 nanoseconds), `sym` (str), `px` (f64): two rows share t=200.
fn quotes() -> DataFrame {
    DataFrame::new_infer_height(vec![
        Column::new("time".into(), [100_i64, 200, 200, 300]),
        Column::new("sym".into(), ["A", "B", "C", "A"]),
        Column::new("px".into(), [1.0_f64, 2.0, 3.0, 4.0]),
    ])
    .unwrap()
}

/// `Stream` is not `Debug`, so `expect_err` cannot be used on a factory result.
fn wiring_error<T>(r: anyhow::Result<T>, expectation: &str) -> String {
    match r {
        Ok(_) => panic!("expected a wiring error: {expectation}"),
        Err(e) => format!("{e:#}"),
    }
}

/// Run a replay and return `(time, [(sym, px)])` per tick.
fn replay(
    source: impl Into<PolarsSource>,
    time_column: &str,
) -> Vec<(NanoTime, Vec<(String, f64)>)> {
    let g = GraphBuilder::new();
    let rows = polars_read(&g, source, time_column, None).unwrap();
    let acc = rows
        .map(|b: &Burst<PolarsRow>| {
            b.iter()
                .map(|r| {
                    let sym = match r.get("sym").unwrap() {
                        AnyValue::StringOwned(s) => s.to_string(),
                        other => panic!("sym: {other:?}"),
                    };
                    (sym, r.get("px").unwrap().extract::<f64>().unwrap())
                })
                .collect::<Vec<_>>()
        })
        .with_time()
        .accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    runner.value(&acc)
}

fn expected_quotes() -> Vec<(NanoTime, Vec<(String, f64)>)> {
    vec![
        (NanoTime::new(100), vec![("A".into(), 1.0)]),
        (
            NanoTime::new(200),
            vec![("B".into(), 2.0), ("C".into(), 3.0)],
        ),
        (NanoTime::new(300), vec![("A".into(), 4.0)]),
    ]
}

#[test]
fn a_frame_replays_at_its_time_column_with_same_instant_rows_in_one_burst() {
    assert_eq!(expected_quotes(), replay(quotes(), "time"));
}

#[test]
fn the_time_column_is_the_tick_time_not_a_row_field() {
    let g = GraphBuilder::new();
    let rows = polars_read(&g, quotes(), "time", None).unwrap();
    let acc = rows.accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    let first = runner.value(&acc)[0][0].clone();
    let names: Vec<&str> = first.schema().iter_names().map(|n| n.as_str()).collect();
    assert_eq!(vec!["sym", "px"], names);
    assert!(first.get("time").is_none());
}

#[test]
fn a_datetime_time_column_is_scaled_to_nanoseconds() {
    for (unit, scale) in [
        (TimeUnit::Nanoseconds, 1),
        (TimeUnit::Microseconds, 1_000),
        (TimeUnit::Milliseconds, 1_000_000),
    ] {
        let raw: Vec<i64> = vec![1, 2, 2, 3];
        let ts = Series::new("ts".into(), raw)
            .cast(&DataType::Datetime(unit, None))
            .unwrap()
            .into_column();
        let df = DataFrame::new_infer_height(vec![
            ts,
            Column::new("sym".into(), ["A", "B", "C", "A"]),
            Column::new("px".into(), [1.0_f64, 2.0, 3.0, 4.0]),
        ])
        .unwrap();
        let times: Vec<NanoTime> = replay(df, "ts").into_iter().map(|(t, _)| t).collect();
        assert_eq!(
            vec![
                NanoTime::new(scale),
                NanoTime::new(2 * scale),
                NanoTime::new(3 * scale)
            ],
            times,
            "{unit:?}"
        );
    }
}

#[test]
fn an_unsigned_time_column_is_nanoseconds() {
    let df = DataFrame::new_infer_height(vec![
        Column::new("t".into(), [100_u64, 200, 200, 300]),
        Column::new("sym".into(), ["A", "B", "C", "A"]),
        Column::new("px".into(), [1.0_f64, 2.0, 3.0, 4.0]),
    ])
    .unwrap();
    assert_eq!(expected_quotes(), replay(df, "t"));
}

#[test]
fn a_missing_time_column_is_a_wiring_error_naming_the_columns() {
    let g = GraphBuilder::new();
    let err = wiring_error(polars_read(&g, quotes(), "ts", None), "missing column");
    assert!(err.contains("no time column 'ts'"), "got: {err}");
    assert!(err.contains("sym"), "lists the actual columns: {err}");
}

#[test]
fn an_unsupported_time_dtype_is_a_wiring_error() {
    let g = GraphBuilder::new();
    let err = wiring_error(polars_read(&g, quotes(), "px", None), "f64 time column");
    assert!(err.contains("f64"), "names the dtype: {err}");
    assert!(err.contains("Datetime"), "lists what is supported: {err}");
}

#[test]
fn a_decreasing_time_column_is_a_wiring_error() {
    let df = DataFrame::new_infer_height(vec![
        Column::new("time".into(), [100_i64, 300, 200]),
        Column::new("px".into(), [1.0_f64, 2.0, 3.0]),
    ])
    .unwrap();
    let g = GraphBuilder::new();
    let err = wiring_error(polars_read(&g, df, "time", None), "unsorted");
    assert!(err.contains("row 2"), "names the row: {err}");
    assert!(err.contains("non-decreasing"), "got: {err}");
}

#[test]
fn null_and_negative_times_are_wiring_errors() {
    let nulls =
        DataFrame::new_infer_height(vec![Column::new("time".into(), [Some(100_i64), None])])
            .unwrap();
    let g = GraphBuilder::new();
    let err = wiring_error(polars_read(&g, nulls, "time", None), "null time");
    assert!(err.contains("row 1") && err.contains("null"), "got: {err}");

    let negative =
        DataFrame::new_infer_height(vec![Column::new("time".into(), [-1_i64, 100])]).unwrap();
    let g = GraphBuilder::new();
    let err = wiring_error(polars_read(&g, negative, "time", None), "negative time");
    assert!(
        err.contains("row 0") && err.contains("negative"),
        "got: {err}"
    );
}

#[test]
fn a_missing_file_is_a_wiring_error() {
    let g = GraphBuilder::new();
    let path = tmp_path("missing.parquet");
    let err = wiring_error(polars_read(&g, &path, "time", None), "missing file");
    assert!(err.contains("polars_read"), "got: {err}");
    assert!(err.contains("missing.parquet"), "names the file: {err}");
}

#[test]
fn an_unknown_extension_is_a_wiring_error() {
    let g = GraphBuilder::new();
    let err = wiring_error(
        polars_read(&g, tmp_path("quotes.csv"), "time", None),
        "unknown extension",
    );
    assert!(
        err.contains("parquet") && err.contains("arrow"),
        "got: {err}"
    );
}

#[test]
fn a_bounded_replay_delivers_the_same_ticks() {
    let g = GraphBuilder::new();
    let rows = polars_read(&g, quotes(), "time", Some(1)).unwrap();
    let acc = rows.map(|b| b.len()).with_time().accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    assert_eq!(
        vec![
            (NanoTime::new(100), 1),
            (NanoTime::new(200), 2),
            (NanoTime::new(300), 1)
        ],
        runner.value(&acc)
    );
}

#[test]
fn collect_rebuilds_the_frame_with_a_leading_time_column() {
    let g = GraphBuilder::new();
    let (_sink, collected) = polars_read(&g, quotes(), "time", None)
        .unwrap()
        .polars_collect();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let df = collected.frame().expect("a frame after a completed run");
    let names: Vec<&str> = df
        .get_column_names()
        .into_iter()
        .map(|n| n.as_str())
        .collect();
    assert_eq!(vec!["time", "sym", "px"], names);
    assert_eq!(
        &DataType::Datetime(TimeUnit::Nanoseconds, None),
        df.column("time").unwrap().dtype()
    );
    let expected = quotes().lazy_free_cast_time().expect("fixture casts");
    assert!(df.equals_missing(&expected), "{df:?}\n!=\n{expected:?}");
}

/// Cast the fixture's integer time column to the sink's `Datetime[ns]`.
trait CastTime {
    fn lazy_free_cast_time(
        self,
    ) -> wingfoil::adapters::polars::polars::prelude::PolarsResult<DataFrame>;
}

impl CastTime for DataFrame {
    fn lazy_free_cast_time(
        mut self,
    ) -> wingfoil::adapters::polars::polars::prelude::PolarsResult<DataFrame> {
        let time = self
            .column("time")?
            .cast(&DataType::Datetime(TimeUnit::Nanoseconds, None))?;
        self.with_column(time)?;
        Ok(self)
    }
}

#[test]
fn collect_can_omit_the_time_column() {
    let g = GraphBuilder::new();
    let (_sink, collected) = polars_read(&g, quotes(), "time", None)
        .unwrap()
        .polars_collect_with_options(PolarsSinkOptions {
            time_column: None,
            ..Default::default()
        });
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    let df = collected.frame().unwrap();
    assert!(df.equals_missing(&quotes().drop("time").unwrap()));
}

#[test]
fn collect_before_any_run_has_no_frame_and_an_empty_run_has_an_empty_one() {
    let g = GraphBuilder::new();
    let empty = quotes().head(Some(0));
    let (_sink, collected) = polars_read(&g, empty, "time", None)
        .unwrap()
        .polars_collect();
    assert!(collected.frame().is_none(), "no run yet");
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    let df = collected.frame().unwrap();
    assert_eq!(0, df.height());
    let names: Vec<&str> = df
        .get_column_names()
        .into_iter()
        .map(|n| n.as_str())
        .collect();
    assert_eq!(vec!["time"], names, "no row, so no row schema");
}

#[test]
fn the_default_sink_options_are_pinned() {
    assert_eq!(
        PolarsSinkOptions {
            time_column: Some("time".to_string()),
            format: None,
        },
        PolarsSinkOptions::default()
    );
}

#[test]
fn a_row_carrying_the_time_column_name_aborts_the_run() {
    // A row that already has a `time` field would collide with the sink's own.
    let schema = Arc::new(Schema::from_iter([("time".into(), DataType::Int64)]));
    let row = PolarsRow::new(schema, vec![AnyValue::Int64(1)]).unwrap();
    let g = GraphBuilder::new();
    let (_sink, _collected) = g.constant(burst![row]).polars_collect();
    let mut runner = g.build();
    let err = runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect_err("duplicate time column");
    let err = format!("{err:#}");
    assert!(
        err.contains("already has a column named 'time'"),
        "got: {err}"
    );
}

#[test]
fn a_row_whose_columns_change_mid_stream_aborts_the_run() {
    let a = Arc::new(Schema::from_iter([("x".into(), DataType::Int64)]));
    let b = Arc::new(Schema::from_iter([("y".into(), DataType::Int64)]));
    let g = GraphBuilder::new();
    let rows = g.constant(burst![
        PolarsRow::new(a, vec![AnyValue::Int64(1)]).unwrap(),
        PolarsRow::new(b, vec![AnyValue::Int64(2)]).unwrap()
    ]);
    let (_sink, _collected) = rows.polars_collect();
    let mut runner = g.build();
    let err = runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect_err("schema change");
    let err = format!("{err:#}");
    assert!(
        err.contains("[\"y\"]") && err.contains("[\"x\"]"),
        "got: {err}"
    );
}

#[test]
fn a_column_whose_dtype_changes_mid_stream_aborts_the_run_but_nulls_do_not() {
    let int = Arc::new(Schema::from_iter([("x".into(), DataType::Int64)]));
    let null = Arc::new(Schema::from_iter([("x".into(), DataType::Null)]));
    let float = Arc::new(Schema::from_iter([("x".into(), DataType::Float64)]));

    // Null, then Int64: the null row does not pin the dtype.
    let g = GraphBuilder::new();
    let (_sink, collected) = g
        .constant(burst![
            PolarsRow::new(null.clone(), vec![AnyValue::Null]).unwrap(),
            PolarsRow::new(int.clone(), vec![AnyValue::Int64(7)]).unwrap()
        ])
        .polars_collect_with_options(PolarsSinkOptions {
            time_column: None,
            ..Default::default()
        });
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    let df = collected.frame().unwrap();
    assert_eq!(&DataType::Int64, df.column("x").unwrap().dtype());
    assert_eq!(1, df.column("x").unwrap().null_count());

    // Int64, then Float64: a real conflict.
    let g = GraphBuilder::new();
    let (_sink, _collected) = g
        .constant(burst![
            PolarsRow::new(int, vec![AnyValue::Int64(1)]).unwrap(),
            PolarsRow::new(float, vec![AnyValue::Float64(1.5)]).unwrap()
        ])
        .polars_collect();
    let mut runner = g.build();
    let err = runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect_err("dtype change");
    let err = format!("{err:#}");
    assert!(
        err.contains("column 'x'") && err.contains("f64"),
        "got: {err}"
    );
}

#[test]
fn a_row_with_the_wrong_number_of_values_is_rejected() {
    let schema = Arc::new(Schema::from_iter([("x".into(), DataType::Int64)]));
    let err = PolarsRow::new(schema, vec![]).unwrap_err();
    assert!(err.to_string().contains("1 column"), "got: {err}");
}

/// Read → transform → write → read back, through each file format.
#[test]
fn a_file_round_trip_reproduces_the_frame_in_both_formats() {
    for (name, format) in [
        ("quotes.parquet", PolarsFormat::Parquet),
        ("quotes.arrow", PolarsFormat::Ipc),
    ] {
        let input = tmp_path(name);
        let output = tmp_path(name);
        format.write(&input, &mut quotes()).unwrap();

        let g = GraphBuilder::new();
        let _sink = polars_read(&g, &input, "time", None)
            .unwrap()
            .polars_write(&output)
            .unwrap();
        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();

        let written = format.read(&output).unwrap();
        let expected = quotes().lazy_free_cast_time().unwrap();
        assert!(written.equals_missing(&expected), "{name}: {written:?}");

        // ...and the written file replays exactly like the original frame.
        assert_eq!(expected_quotes(), replay(&output, "time"), "{name}");
        let _ = std::fs::remove_file(&input);
        let _ = std::fs::remove_file(&output);
    }
}

#[test]
fn an_explicit_format_overrides_the_extension() {
    let path = tmp_path("quotes.bin");
    PolarsFormat::Ipc.write(&path, &mut quotes()).unwrap();
    assert_eq!(
        expected_quotes(),
        replay(PolarsSource::Ipc(path.clone()), "time")
    );

    let out = tmp_path("out.bin");
    let g = GraphBuilder::new();
    let _sink = polars_read(&g, quotes(), "time", None)
        .unwrap()
        .polars_write_with_options(
            &out,
            PolarsSinkOptions {
                format: Some(PolarsFormat::Parquet),
                ..Default::default()
            },
        )
        .unwrap();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    assert_eq!(4, PolarsFormat::Parquet.read(&out).unwrap().height());
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn write_to_an_unknown_extension_or_unwritable_path_is_a_wiring_error() {
    let g = GraphBuilder::new();
    let rows = polars_read(&g, quotes(), "time", None).unwrap();
    let err = wiring_error(rows.polars_write(tmp_path("out.csv")), "unknown extension");
    assert!(err.contains("polars_write"), "got: {err}");

    let dir = tmp_path("no_such_dir").join("out.parquet");
    let err = wiring_error(rows.polars_write(&dir), "unwritable path");
    assert!(err.contains("no_such_dir"), "names the path: {err}");
}

#[test]
fn a_single_row_stream_sinks_without_wrapping() {
    let schema = Arc::new(Schema::from_iter([("x".into(), DataType::Int64)]));
    let g = GraphBuilder::new();
    let row = PolarsRow::new(schema, vec![AnyValue::Int64(3)]).unwrap();
    let (_sink, collected) = g.constant(row).polars_collect();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    assert_eq!(1, collected.frame().unwrap().height());
}

#[test]
fn a_second_run_collects_only_its_own_rows() {
    // The sink's buffer is per-run state, so a re-run does not append.
    let schema = Arc::new(Schema::from_iter([("x".into(), DataType::Int64)]));
    let g = GraphBuilder::new();
    let row = PolarsRow::new(schema, vec![AnyValue::Int64(3)]).unwrap();
    let (_sink, collected) = g.constant(row).polars_collect();
    let mut runner = g.build();
    for _ in 0..2 {
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        assert_eq!(1, collected.frame().unwrap().height());
    }
}

/// Entries in `path`'s directory that are write temps of `path`.
fn temp_siblings(path: &std::path::Path) -> Vec<std::ffi::OsString> {
    let prefix = format!(".{}.", path.file_name().unwrap().to_string_lossy());
    std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name())
        .filter(|n| n.to_string_lossy().starts_with(&prefix))
        .collect()
}

#[test]
fn an_aborted_write_replaces_the_file_whole() {
    // Wiring must not truncate the target (a zero-byte Parquet file is
    // invalid). `stop` runs after an abort too (the `Op::stop` contract), so
    // the aborted run writes the rows that reached the sink before the abort —
    // as a complete file, through a temp sibling and a rename.
    for (name, format) in [
        ("prev.parquet", PolarsFormat::Parquet),
        ("prev.arrow", PolarsFormat::Ipc),
    ] {
        let path = tmp_path(name);
        format.write(&path, &mut quotes()).unwrap();

        let g = GraphBuilder::new();
        let _sink = polars_read(&g, quotes(), "time", None)
            .unwrap()
            .try_map(|b: &Burst<PolarsRow>| {
                // Abort at t=200, after the t=100 row reached the sink.
                if b.iter().any(
                    |r| matches!(r.get("sym"), Some(AnyValue::StringOwned(s)) if s.as_str() == "C"),
                ) {
                    anyhow::bail!("abort mid-run");
                }
                Ok(b.clone())
            })
            .polars_write(&path)
            .unwrap();
        // Wired, not yet run: the previous file is untouched.
        assert!(
            format.read(&path).unwrap().equals_missing(&quotes()),
            "{name}: wiring touched the target"
        );

        let mut runner = g.build();
        let err = runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .expect_err("the run aborts");
        assert!(format!("{err:#}").contains("abort mid-run"), "{err:#}");

        let written = format.read(&path).unwrap();
        let expected = quotes().slice(0, 1).lazy_free_cast_time().unwrap();
        assert!(written.equals_missing(&expected), "{name}: {written:?}");
        assert!(temp_siblings(&path).is_empty(), "{name}: temp left behind");
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn a_failed_write_leaves_no_temp_file() {
    // The rename fails (the target became a directory after wiring): the run
    // errors naming it, the directory is untouched and the temp is removed.
    let path = tmp_path("becomes_a_dir.parquet");
    let g = GraphBuilder::new();
    let _sink = polars_read(&g, quotes(), "time", None)
        .unwrap()
        .polars_write(&path)
        .unwrap();
    std::fs::create_dir(&path).unwrap();
    let mut runner = g.build();
    let err = runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .expect_err("rename over a directory fails");
    assert!(format!("{err:#}").contains("renaming"), "{err:#}");
    assert!(path.is_dir());
    assert!(temp_siblings(&path).is_empty(), "temp left behind");
    let _ = std::fs::remove_dir(&path);
}

#[test]
fn a_column_null_in_every_row_is_written_as_null_dtype() {
    // No row ever pins the dtype, so the column keeps polars' `Null` dtype —
    // collected and through a Parquet round trip.
    let schema = Arc::new(Schema::from_iter([
        ("x".into(), DataType::Int64),
        ("y".into(), DataType::Null),
    ]));
    let rows = || {
        burst![
            PolarsRow::new(schema.clone(), vec![AnyValue::Int64(1), AnyValue::Null]).unwrap(),
            PolarsRow::new(schema.clone(), vec![AnyValue::Int64(2), AnyValue::Null]).unwrap()
        ]
    };
    let options = || PolarsSinkOptions {
        time_column: None,
        ..Default::default()
    };

    let g = GraphBuilder::new();
    let (_sink, collected) = g.constant(rows()).polars_collect_with_options(options());
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    let df = collected.frame().unwrap();
    assert_eq!(&DataType::Int64, df.column("x").unwrap().dtype());
    assert_eq!(&DataType::Null, df.column("y").unwrap().dtype());
    assert_eq!(2, df.column("y").unwrap().null_count());

    let path = tmp_path("nulls.parquet");
    let g = GraphBuilder::new();
    let _sink = g
        .constant(rows())
        .polars_write_with_options(&path, options())
        .unwrap();
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    let written = PolarsFormat::Parquet.read(&path).unwrap();
    assert_eq!(&DataType::Null, written.column("y").unwrap().dtype());
    assert_eq!(2, written.column("y").unwrap().null_count());
    let _ = std::fs::remove_file(&path);
}

// --- Chunked file replay ------------------------------------------------------
//
// A file is read one Parquet row group / IPC record batch at a time during the
// run, so these write files in several small chunks and pin that the chunk
// boundaries are invisible to the graph: bursts that straddle them arrive
// whole, ordering is checked across them, and a bad later chunk only surfaces
// once the replay reaches it.

use std::cell::RefCell;
use std::rc::Rc;

use wingfoil::adapters::polars::polars::prelude::{
    IpcWriter, ParquetReader, ParquetWriter, SerReader, SerWriter,
};

/// A `time` / `sym` / `px` frame in `chunk`-row chunks (kept as separate
/// polars chunks, which the writers below turn into row groups / batches).
fn chunked_quotes(times: &[i64], chunk: usize) -> DataFrame {
    let times: Vec<Option<i64>> = times.iter().copied().map(Some).collect();
    chunked_quotes_with_nulls(&times, chunk)
}

/// [`chunked_quotes`] with nullable times.
fn chunked_quotes_with_nulls(times: &[Option<i64>], chunk: usize) -> DataFrame {
    let syms = ["A", "B", "C", "D", "E", "F", "G", "H", "I", "J"];
    let mut parts = times.chunks(chunk).enumerate().map(|(k, ts)| {
        let base = k * chunk;
        DataFrame::new_infer_height(vec![
            Column::new("time".into(), ts.to_vec()),
            Column::new(
                "sym".into(),
                (0..ts.len()).map(|i| syms[base + i]).collect::<Vec<_>>(),
            ),
            Column::new(
                "px".into(),
                (0..ts.len())
                    .map(|i| (base + i + 1) as f64)
                    .collect::<Vec<_>>(),
            ),
        ])
        .unwrap()
    });
    let mut df = parts.next().unwrap();
    for part in parts {
        df.vstack_mut(&part).unwrap();
    }
    df
}

/// Write `df` to `path` with one row group / record batch per `chunk` rows.
fn write_chunked(format: PolarsFormat, path: &std::path::Path, mut df: DataFrame, chunk: usize) {
    let file = std::fs::File::create(path).unwrap();
    match format {
        PolarsFormat::Parquet => {
            ParquetWriter::new(file)
                .with_row_group_size(Some(chunk))
                .finish(&mut df)
                .unwrap();
            // The layout the tests rely on: one row group per chunk.
            let groups = ParquetReader::new(std::fs::File::open(path).unwrap())
                .get_metadata()
                .unwrap()
                .row_groups
                .len();
            assert_eq!(df.height().div_ceil(chunk), groups, "row groups");
        }
        PolarsFormat::Ipc => IpcWriter::new(file).finish(&mut df).unwrap(),
    }
}

/// Replay `path`, recording `(time, [(sym, px)])` per tick as the run goes, so
/// the ticks before an abort are kept. Returns them with the run's result.
///
/// On an abort, the tick *just* before the error is not among them: the
/// historical receiver reads one message past a group to know the group is
/// complete, and when that message is the error it aborts with the group still
/// open. So "delivered before the abort" is every group but the last one read.
fn replay_recorded(
    path: &std::path::Path,
    buffer_size: Option<usize>,
) -> (Vec<(NanoTime, Vec<(String, f64)>)>, anyhow::Result<()>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let g = GraphBuilder::new();
    let sink = seen.clone();
    let _sink = polars_read(&g, path, "time", buffer_size)
        .expect("wiring reads only the footer")
        .with_time()
        .for_each(move |(t, b): &(NanoTime, Burst<PolarsRow>)| {
            let rows = b
                .iter()
                .map(|r| {
                    let sym = match r.get("sym").unwrap() {
                        AnyValue::StringOwned(s) => s.to_string(),
                        other => panic!("sym: {other:?}"),
                    };
                    (sym, r.get("px").unwrap().extract::<f64>().unwrap())
                })
                .collect();
            sink.borrow_mut().push((*t, rows));
            Ok(())
        });
    let mut runner = g.build();
    let result = runner.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
    let seen = seen.borrow().clone();
    (seen, result.map(|_| ()))
}

fn formats(stem: &str) -> [(PathBuf, PolarsFormat); 2] {
    [
        (tmp_path(&format!("{stem}.parquet")), PolarsFormat::Parquet),
        (tmp_path(&format!("{stem}.arrow")), PolarsFormat::Ipc),
    ]
}

#[test]
fn a_burst_straddling_a_chunk_boundary_arrives_as_one_burst() {
    // Chunks of two rows: [100 200 | 200 300 | 300 300 | 400]. The t=200 burst
    // spans chunks 0-1, the t=300 burst spans chunks 1-2.
    let times = [100_i64, 200, 200, 300, 300, 300, 400];
    let expected = vec![
        (NanoTime::new(100), vec![("A".into(), 1.0)]),
        (
            NanoTime::new(200),
            vec![("B".into(), 2.0), ("C".into(), 3.0)],
        ),
        (
            NanoTime::new(300),
            vec![("D".into(), 4.0), ("E".into(), 5.0), ("F".into(), 6.0)],
        ),
        (NanoTime::new(400), vec![("G".into(), 7.0)]),
    ];
    for (path, format) in formats("straddle") {
        write_chunked(format, &path, chunked_quotes(&times, 2), 2);
        for buffer_size in [Some(1), None] {
            let (seen, result) = replay_recorded(&path, buffer_size);
            result.unwrap_or_else(|e| panic!("{format:?} {buffer_size:?}: {e:#}"));
            assert_eq!(expected, seen, "{format:?} {buffer_size:?}");
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn a_decrease_across_a_chunk_boundary_aborts_the_run_naming_the_row() {
    // [100 200 | 150 300]: row 2 (the first of chunk 1) is before row 1.
    for (path, format) in formats("decrease") {
        write_chunked(format, &path, chunked_quotes(&[100, 200, 150, 300], 2), 2);
        for buffer_size in [Some(1), None] {
            let (seen, result) = replay_recorded(&path, buffer_size);
            let err = match result {
                Ok(()) => panic!("{format:?} {buffer_size:?}: the run should abort"),
                Err(e) => format!("{e:#}"),
            };
            assert!(
                err.contains("row 2 time 150ns is before row 1 time 200ns")
                    && err.contains("non-decreasing"),
                "{format:?}: {err}"
            );
            assert!(
                err.contains(&*path.to_string_lossy()),
                "{format:?}: names the file: {err}"
            );
            // Chunk 0 was replayed before chunk 1 was read (its last group,
            // t=200, still open when the error arrived — see `replay_recorded`).
            assert_eq!(
                vec![(NanoTime::new(100), vec![("A".into(), 1.0)])],
                seen,
                "{format:?} {buffer_size:?}"
            );
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn a_bad_time_in_a_later_chunk_does_not_stop_the_file_wiring() {
    // The laziness probe: a whole-file load validated every time at wiring and
    // refused this file there. Streaming, it wires, replays the good chunks,
    // then aborts on the null in chunk 2 (and on the negative, separately).
    for (bad, message) in [
        (None, "row 4 has a null time"),
        (Some(-5), "row 4 has time -5ns"),
    ] {
        let times = vec![
            Some(100_i64),
            Some(200),
            Some(300),
            Some(400),
            bad,
            Some(600),
        ];
        for (path, format) in formats("late_bad") {
            write_chunked(format, &path, chunked_quotes_with_nulls(&times, 2), 2);
            let (seen, result) = replay_recorded(&path, Some(1));
            let err = match result {
                Ok(()) => panic!("{format:?}: the run should abort"),
                Err(e) => format!("{e:#}"),
            };
            assert!(err.contains(message), "{format:?}: {err}");
            // Chunks 0 and 1 were replayed (t=400 still open at the abort).
            let times: Vec<NanoTime> = seen.iter().map(|(t, _)| *t).collect();
            assert_eq!(
                vec![NanoTime::new(100), NanoTime::new(200), NanoTime::new(300)],
                times,
                "{format:?}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[test]
fn a_corrupt_later_row_group_is_not_read_at_wiring() {
    // Overwrite the last row group's column chunks with garbage. A whole-file
    // load fails to decode it at wiring; the streaming read never touches it
    // until the replay gets there, so the first two row groups are delivered.
    let path = tmp_path("corrupt.parquet");
    write_chunked(
        PolarsFormat::Parquet,
        &path,
        chunked_quotes(&[100, 200, 300, 400, 500, 600], 2),
        2,
    );
    let ranges: Vec<std::ops::Range<u64>> = {
        let mut reader = ParquetReader::new(std::fs::File::open(&path).unwrap());
        let metadata = reader.get_metadata().unwrap();
        metadata
            .row_groups
            .last()
            .unwrap()
            .byte_ranges_iter()
            .collect()
    };
    let mut bytes = std::fs::read(&path).unwrap();
    for range in ranges {
        for b in &mut bytes[range.start as usize..range.end as usize] {
            *b = 0xA5;
        }
    }
    std::fs::write(&path, bytes).unwrap();

    let (seen, result) = replay_recorded(&path, Some(1));
    let err = match result {
        Ok(()) => panic!("the run should abort on the corrupt row group"),
        Err(e) => format!("{e:#}"),
    };
    assert!(
        err.contains("row group 2") && err.contains("corrupt.parquet"),
        "names the chunk and the file: {err}"
    );
    // Row groups 0 and 1 were replayed (t=400 still open at the abort).
    let times: Vec<NanoTime> = seen.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        vec![NanoTime::new(100), NanoTime::new(200), NanoTime::new(300)],
        times
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_file_with_a_bad_time_column_schema_is_still_a_wiring_error() {
    // The footer carries the schema, so a missing time column or an
    // unsupported dtype still fails before the run, for a file too.
    for (path, format) in formats("schema") {
        format.write(&path, &mut quotes()).unwrap();
        let g = GraphBuilder::new();
        let err = wiring_error(polars_read(&g, &path, "ts", None), "missing column");
        assert!(err.contains("no time column 'ts'"), "{format:?}: {err}");
        assert!(err.contains("sym"), "{format:?}: lists the columns: {err}");
        let err = wiring_error(polars_read(&g, &path, "px", None), "f64 time column");
        assert!(
            err.contains("f64") && err.contains("Datetime"),
            "{format:?}: {err}"
        );
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn a_frame_longer_than_one_chunk_replays_across_the_boundary() {
    // In-memory frames are sliced into 64Ki-row chunks. Rows come in pairs
    // sharing a time — [0], [1 2], [3 4], … — so the pair (65535, 65536)
    // straddles the first boundary.
    const ROWS: usize = 64 * 1024 * 2 + 3;
    let times: Vec<i64> = (0..ROWS as i64).map(|i| (i + 1) / 2).collect();
    let df = DataFrame::new_infer_height(vec![
        Column::new("time".into(), times),
        Column::new("px".into(), (0..ROWS).map(|i| i as f64).collect::<Vec<_>>()),
    ])
    .unwrap();
    let g = GraphBuilder::new();
    let ticks = Rc::new(RefCell::new(Vec::new()));
    let sink = ticks.clone();
    let _sink = polars_read(&g, df, "time", Some(4))
        .unwrap()
        .with_time()
        .for_each(move |(t, b): &(NanoTime, Burst<PolarsRow>)| {
            let px: Vec<f64> = b
                .iter()
                .map(|r| r.get("px").unwrap().extract::<f64>().unwrap())
                .collect();
            sink.borrow_mut().push((*t, px));
            Ok(())
        });
    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();
    let ticks = ticks.borrow();
    assert_eq!(ROWS / 2 + 1, ticks.len());
    assert_eq!((NanoTime::new(0), vec![0.0]), ticks[0]);
    assert_eq!(
        (NanoTime::new(32_768), vec![65_535.0, 65_536.0]),
        ticks[32_768]
    );
    assert!(
        ticks
            .iter()
            .enumerate()
            .all(|(k, (t, px))| u64::from(*t) == k as u64 && px.len() == (k.min(1) + 1)),
        "every tick after the first is a pair at its own instant"
    );
}

#[test]
fn a_decrease_across_a_frame_chunk_boundary_is_a_wiring_error() {
    const ROWS: usize = 64 * 1024 + 1;
    let mut times: Vec<i64> = (0..ROWS as i64).collect();
    times[ROWS - 1] = 7; // the first row of the second chunk goes backwards
    let df = DataFrame::new_infer_height(vec![Column::new("time".into(), times)]).unwrap();
    let g = GraphBuilder::new();
    let err = wiring_error(polars_read(&g, df, "time", None), "unsorted");
    assert!(
        err.contains(&format!(
            "row {} time 7ns is before row {}",
            ROWS - 1,
            ROWS - 2
        )),
        "got: {err}"
    );
}
