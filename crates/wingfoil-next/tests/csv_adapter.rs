//! CSV adapter (Phase 4): a serde-typed historical replay **source** and a file
//! **sink**. These tests port the classic `wingfoil::adapters::csv` unit tests
//! as parity tests (all-rows / single-burst emission, same-timestamp grouping,
//! missing-file and malformed-row error handling) and add an end-to-end
//! read → transform → write round trip asserting the output contents and the
//! replay ordering/timestamps.

#![cfg(feature = "csv")]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use wingfoil_next::adapters::csv::{CsvSinkOps, csv_read};
use wingfoil_next::prelude::*;
use wingfoil_next::{NanoTime, RunFor, RunMode};

/// The classic parity record: a positional `(time, value)` tuple.
type Record = (NanoTime, u32);

fn get_time(r: &Record) -> NanoTime {
    r.0
}

/// Stage a CSV fixture in a uniquely-named temp file. Uniqueness keys on the
/// pid *and* a process-wide `AtomicU64` counter (mirroring `tests/lines_adapter.rs`)
/// as well as the call-site `name`, so parallel runs — and any future test that
/// reuses a `name` — never collide.
fn write_tmp(name: &str, contents: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("wf_next_csv_{}_{}_{name}", std::process::id(), n));
    std::fs::write(&path, contents).expect("write temp fixture");
    path
}

/// Read output back as `\n`-normalized lines (the csv crate terminates rows
/// with CRLF; normalizing keeps the assertions terminator-agnostic).
fn output_lines(path: &PathBuf) -> Vec<String> {
    std::fs::read_to_string(path)
        .expect("read output")
        .replace('\r', "")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Classic `csv_read_emits_all_rows` + `csv_read_each_row_is_single_burst`:
/// six distinct-timestamp rows replay as six single-element bursts, every row
/// (including the last) delivered.
#[test]
fn csv_read_emits_all_rows_each_a_single_burst() {
    let path = write_tmp(
        "all_rows.csv",
        "1001,1\n1002,2\n1003,3\n1004,4\n1005,5\n1006,6\n",
    );

    let g = GraphBuilder::new();
    let rows = csv_read(&g, &path, get_time, false, None).unwrap();
    let acc = rows.with_time().accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let ticks = r.value(&acc);
    // Six distinct timestamps → six bursts, each holding exactly one row.
    assert_eq!(ticks.len(), 6);
    assert!(ticks.iter().all(|(_, b)| b.len() == 1));
    // Every value present, in order.
    let values: Vec<u32> = ticks
        .iter()
        .flat_map(|(_, b)| b.iter().map(|r| r.1))
        .collect();
    assert_eq!(values, vec![1, 2, 3, 4, 5, 6]);
    // Delivered at their own timestamps.
    let times: Vec<NanoTime> = ticks.iter().map(|(t, _)| *t).collect();
    assert_eq!(times, (1001..=1006).map(NanoTime::new).collect::<Vec<_>>());
}

/// Classic `csv_read_groups_same_timestamp_into_one_burst`: timestamps
/// 1001, 1002, 1003(×2), 1004 → 4 ticks, the two 1003 rows in one atomic burst.
#[test]
fn csv_read_groups_same_timestamp_into_one_burst() {
    let path = write_tmp("multi.csv", "1001,1\n1002,2\n1003,3\n1003,3\n1004,4\n");

    let g = GraphBuilder::new();
    let rows = csv_read(&g, &path, get_time, false, None).unwrap();
    let acc = rows.with_time().accumulate();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let ticks = r.value(&acc);
    assert_eq!(ticks.len(), 4, "four distinct timestamps");
    let burst_1003 = ticks
        .iter()
        .find(|(t, _)| *t == NanoTime::new(1003))
        .expect("a burst at t=1003");
    assert_eq!(burst_1003.1.len(), 2, "both 1003 rows ride one burst");
}

/// Classic `csv_read_missing_file_returns_error_with_context`: a missing file
/// surfaces a contextual error at wiring time rather than panicking.
#[test]
fn csv_read_missing_file_returns_error_with_context() {
    let g = GraphBuilder::new();
    let result = csv_read::<Record, _>(
        &g,
        std::env::temp_dir().join("wf_next_csv_does_not_exist.csv"),
        get_time,
        false,
        None,
    );
    // `Stream` isn't `Debug`, so match rather than `expect_err`.
    let err = match result {
        Ok(_) => panic!("expected an error for a missing file"),
        Err(e) => e,
    };
    assert!(
        format!("{err:#}").contains("csv_read: failed to open"),
        "unexpected error message: {err:#}"
    );
}

/// Classic `csv_read_malformed_row_surfaces_error_not_panic`: a row that fails
/// to deserialize aborts the run with a contextual error rather than panicking.
/// (Deviation from classic: surfaced through the channel at the start of the
/// replay rather than mid-stream — same observable outcome and message.)
#[test]
fn csv_read_malformed_row_surfaces_error_not_panic() {
    let path = write_tmp("malformed.csv", "1001,1\n1002,notanumber\n1003,3\n");

    let g = GraphBuilder::new();
    let rows = csv_read::<Record, _>(&g, &path, get_time, false, None)
        .expect("file opens fine; the error is in a row");
    let _acc = rows.with_time().accumulate();
    let mut r = g.build();
    let result = r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);

    let err = result.expect_err("expected a deserialize error");
    assert!(
        format!("{err:#}").contains("failed to deserialize row"),
        "unexpected error message: {err:#}"
    );
}

/// End-to-end round trip: read a CSV, transform every row (double the value) on
/// the graph clock, and write it back. Asserts both the replay
/// ordering/timestamps and the exact output-file contents.
#[test]
fn csv_round_trip_read_transform_write() {
    let path = write_tmp("round_in.csv", "1001,1\n1002,2\n1003,3\n1003,3\n1004,4\n");
    let out = write_tmp("round_out.csv", "");

    let g = GraphBuilder::new();
    let rows = csv_read(&g, &path, get_time, false, None).unwrap();
    // Transform: double each value, preserving the burst grouping.
    let doubled = rows.map(|b: &Burst<Record>| {
        b.iter()
            .map(|(t, v)| (*t, v * 2))
            .collect::<Burst<Record>>()
    });
    let acc = doubled.with_time().accumulate();
    let _sink = doubled.csv_write(&out).unwrap();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    // Replay ordering/timestamps: four ticks, the 1003 pair in one burst.
    let ticks = r.value(&acc);
    let times: Vec<NanoTime> = ticks.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        times,
        vec![
            NanoTime::new(1001),
            NanoTime::new(1002),
            NanoTime::new(1003),
            NanoTime::new(1004),
        ]
    );

    // Output contents: one `(time, orig_time, doubled_value)` row per record; a
    // tuple record has no named fields, so there is no header row.
    assert_eq!(
        output_lines(&out),
        vec![
            "1001,1001,2",
            "1002,1002,4",
            "1003,1003,6",
            "1003,1003,6",
            "1004,1004,8",
        ]
    );
}

/// The single-value convenience impl (`CsvSinkOps for Stream<T>`) auto-wraps
/// each value into a one-element burst, so a plain `Stream<Record>` writes
/// without a manual `.map(|v| burst![v])`.
#[test]
fn single_value_stream_uses_the_convenience_sink() {
    let path = write_tmp("sv_in.csv", "1001,1\n1002,2\n");
    let out = write_tmp("sv_out.csv", "");

    let g = GraphBuilder::new();
    // `collapse` turns Stream<Burst<Record>> into a plain Stream<Record>.
    let each: Stream<Record> = csv_read(&g, &path, get_time, false, None)
        .unwrap()
        .collapse();
    let _sink = each.csv_write(&out).unwrap();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    assert_eq!(output_lines(&out), vec!["1001,1001,1", "1002,1002,2"]);
}

/// A named-struct record exercises the header path: the sink writes a leading
/// `time` column plus the record's serde field names, then one row per record.
#[test]
fn csv_sink_writes_header_for_named_struct() {
    #[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
    struct Quote {
        timestamp: u64,
        price: u32,
    }

    let path = write_tmp("struct_in.csv", "100,10\n200,20\n");
    let out = write_tmp("struct_out.csv", "");

    let g = GraphBuilder::new();
    let rows = csv_read(
        &g,
        &path,
        |q: &Quote| NanoTime::new(q.timestamp),
        false,
        None,
    )
    .unwrap();
    let _sink = rows.csv_write(&out).unwrap();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    assert_eq!(
        output_lines(&out),
        vec!["time,timestamp,price", "100,100,10", "200,200,20"],
    );
}

/// A bounded `buffer_size` replays byte-identically to the unbounded one — the
/// back-pressure paces the file read without changing what the graph sees — and
/// a same-time burst **larger than the bound** must not deadlock (historical
/// back-pressure counts timestamp-groups, so the whole burst rides one slot).
#[test]
fn csv_read_bounded_is_deterministic_and_survives_large_bursts() {
    // 20 rows all sharing t=1000 (one group, far larger than a bound of 2), then
    // three later distinct-timestamp rows.
    let mut contents = String::new();
    for v in 0..20u32 {
        contents.push_str(&format!("1000,{v}\n"));
    }
    contents.push_str("1001,100\n1002,101\n1003,102\n");
    let path = write_tmp("bounded.csv", &contents);

    let read_with = |buffer: Option<usize>| -> Vec<(NanoTime, Vec<u32>)> {
        let g = GraphBuilder::new();
        let rows = csv_read(&g, &path, get_time, false, buffer).unwrap();
        let acc = rows.with_time().accumulate();
        let mut r = g.build();
        r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .unwrap();
        r.value(&acc)
            .into_iter()
            .map(|(t, b)| (t, b.iter().map(|r| r.1).collect()))
            .collect()
    };

    let unbounded = read_with(None);
    // 4 distinct timestamps; the t=1000 burst holds all 20 rows.
    assert_eq!(unbounded.len(), 4);
    assert_eq!(unbounded[0].0, NanoTime::new(1000));
    assert_eq!(unbounded[0].1, (0..20u32).collect::<Vec<_>>());
    // Bounded (Some(2), below the 20-row burst) must not deadlock and must match.
    assert_eq!(unbounded, read_with(Some(2)));
    assert_eq!(unbounded, read_with(Some(5)));
}

/// `csv_write_with_header` names the columns explicitly, for a record whose
/// shape is only known at runtime.
///
/// A positional record (here a `Vec<String>` built from a caller-supplied
/// column list — the shape the Python binding marshals into) has no serde field
/// names, so plain `csv_write` writes no header. This is the escape hatch.
#[test]
fn write_with_header_names_the_columns_explicitly() {
    let path = write_tmp("with_header.csv", "");
    let header = vec!["sym".to_string(), "px".to_string()];

    let g = GraphBuilder::new();
    let rows: Stream<Vec<String>> = g
        .ticker(std::time::Duration::from_secs(1))
        .count()
        .map(|i: &u64| vec![format!("S{i}"), format!("{i}.5")]);
    rows.csv_write_with_header(&path, &header).unwrap();

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(2))
        .unwrap();
    drop(runner);

    let lines = output_lines(&path);
    assert_eq!("time,sym,px", lines[0]);
    assert_eq!(3, lines.len(), "header + 2 rows, got {lines:?}");
    assert!(lines[1].ends_with("S1,1.5"), "{:?}", lines[1]);
    std::fs::remove_file(&path).ok();
}

/// An empty header writes no header row — the positional-tuple behaviour, so a
/// caller that genuinely wants a headerless file can ask for one.
#[test]
fn write_with_an_empty_header_writes_no_header_row() {
    let path = write_tmp("empty_header.csv", "");

    let g = GraphBuilder::new();
    let rows: Stream<Vec<String>> = g
        .ticker(std::time::Duration::from_secs(1))
        .count()
        .map(|i: &u64| vec![format!("S{i}")]);
    rows.csv_write_with_header(&path, &[]).unwrap();

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();
    drop(runner);

    let lines = output_lines(&path);
    assert!(
        !lines[0].starts_with("time,"),
        "expected no header, got {lines:?}"
    );
    std::fs::remove_file(&path).ok();
}

/// Plain `csv_write` on the same positional record writes *no* header — the
/// gap `csv_write_with_header` exists to close.
#[test]
fn plain_write_gives_a_positional_record_no_header() {
    let path = write_tmp("no_header.csv", "");

    let g = GraphBuilder::new();
    let rows: Stream<Vec<String>> = g
        .ticker(std::time::Duration::from_secs(1))
        .count()
        .map(|i: &u64| vec![format!("S{i}")]);
    rows.csv_write(&path).unwrap();

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
        .unwrap();
    drop(runner);

    let lines = output_lines(&path);
    assert!(
        !lines[0].starts_with("time,sym"),
        "expected no header, got {lines:?}"
    );
    std::fs::remove_file(&path).ok();
}
