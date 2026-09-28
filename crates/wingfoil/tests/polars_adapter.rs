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
