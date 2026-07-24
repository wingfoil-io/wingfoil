//! CSV adapter (Phase 4): a serde-typed historical replay **source** and a file
//! **sink**. These tests port the classic `wingfoil::adapters::csv` unit tests
//! as parity tests (all-rows / single-burst emission, same-timestamp grouping,
//! missing-file and malformed-row error handling) and add an end-to-end
//! read → transform → write round trip asserting the output contents and the
//! replay ordering/timestamps.

#![cfg(feature = "csv")]

use std::path::PathBuf;

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::adapters::csv::{CsvSinkOps, csv_read};
use wingfoil_next::prelude::*;

/// The classic parity record: a positional `(time, value)` tuple.
type Record = (NanoTime, u32);

fn get_time(r: &Record) -> NanoTime {
    r.0
}

/// Stage a CSV fixture in a uniquely-named temp file (unique per test so
/// parallel runs don't collide).
fn write_tmp(name: &str, contents: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("wf_next_csv_{}_{name}", std::process::id()));
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
    let rows = csv_read(&g, &path, get_time, false).unwrap();
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
    let rows = csv_read(&g, &path, get_time, false).unwrap();
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
    let rows = csv_read::<Record, _>(&g, &path, get_time, false)
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
    let rows = csv_read(&g, &path, get_time, false).unwrap();
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
    let rows = csv_read(&g, &path, |q: &Quote| NanoTime::new(q.timestamp), false).unwrap();
    let _sink = rows.csv_write(&out).unwrap();
    let mut r = g.build();
    r.run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    assert_eq!(
        output_lines(&out),
        vec!["time,timestamp,price", "100,100,10", "200,200,20"],
    );
}
