//! The dependency-free line-oriented file adapter: deterministic historical
//! replay of a file through a graph, a transform, and a file sink — asserting
//! both the written output and the replay ordering / timestamps.

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use wingfoil::{NanoTime, RunFor, RunMode};
use wingfoil_next::Burst;
use wingfoil_next::adapters::lines::{LinesSinkOps, replay_lines, replay_lines_scheduled};
use wingfoil_next::prelude::*;

/// A unique temp path per call, so parallel tests never collide.
fn temp_path(tag: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "wingfoil_lines_{}_{}_{}.txt",
        tag,
        std::process::id(),
        n
    ))
}

/// End to end: replay a file through the source, transform each record, write
/// it back through the sink, and assert the output file's exact contents.
#[test]
fn replay_transform_and_sink_roundtrips_through_files() {
    let input = temp_path("in");
    let output = temp_path("out");
    fs::write(&input, "alpha\nbravo\ncharlie\ndelta\n").unwrap();

    let g = GraphBuilder::new();
    let lines = replay_lines(&g, &input).unwrap();
    let shouted = lines.map(|burst: &Burst<String>| {
        burst
            .iter()
            .map(|s| s.to_uppercase())
            .collect::<Burst<String>>()
    });
    shouted.write_lines(&output).unwrap();

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let written = fs::read_to_string(&output).unwrap();
    assert_eq!(written, "ALPHA\nBRAVO\nCHARLIE\nDELTA\n");

    fs::remove_file(&input).ok();
    fs::remove_file(&output).ok();
}

/// The historical replay is deterministic: each record is delivered on the
/// graph clock at `base + i * step`, in order, one line per burst.
#[test]
fn replay_is_scheduled_deterministically_on_the_graph_clock() {
    let input = temp_path("sched");
    fs::write(&input, "one\ntwo\nthree\n").unwrap();

    let base = NanoTime::new(1_000);
    let step = std::time::Duration::from_nanos(10);

    let g = GraphBuilder::new();
    let lines = replay_lines_scheduled(&g, &input, base, step).unwrap();
    // Pair each burst with the engine time it was delivered at.
    let stamped = lines.with_time().accumulate();

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let got: Vec<(NanoTime, Vec<String>)> = runner
        .value(&stamped)
        .into_iter()
        .map(|(t, b)| (t, b.iter().cloned().collect()))
        .collect();

    assert_eq!(
        got,
        vec![
            (NanoTime::new(1_000), vec!["one".to_string()]),
            (NanoTime::new(1_010), vec!["two".to_string()]),
            (NanoTime::new(1_020), vec!["three".to_string()]),
        ],
    );

    fs::remove_file(&input).ok();
}

/// A zero step collapses same-instant records into one atomic burst — the
/// historical burst model (never split, never coalesced).
#[test]
fn zero_step_groups_records_into_one_burst() {
    let input = temp_path("burst");
    fs::write(&input, "a\nb\nc\n").unwrap();

    let g = GraphBuilder::new();
    let lines = replay_lines_scheduled(
        &g,
        &input,
        NanoTime::ZERO,
        std::time::Duration::from_nanos(0),
    )
    .unwrap();
    let stamped = lines.with_time().accumulate();

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    let got: Vec<(NanoTime, Vec<String>)> = runner
        .value(&stamped)
        .into_iter()
        .map(|(t, b)| (t, b.iter().cloned().collect()))
        .collect();

    // All three records ride a single burst at time zero.
    assert_eq!(
        got,
        vec![(
            NanoTime::ZERO,
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
        )],
    );

    fs::remove_file(&input).ok();
}

/// An append sink adds to existing content rather than truncating.
#[test]
fn append_sink_preserves_existing_content() {
    let input = temp_path("append_in");
    let output = temp_path("append_out");
    fs::write(&input, "x\ny\n").unwrap();
    fs::write(&output, "header\n").unwrap();

    let g = GraphBuilder::new();
    let lines = replay_lines(&g, &input).unwrap();
    lines.append_lines(&output).unwrap();

    let mut runner = g.build();
    runner
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
        .unwrap();

    assert_eq!(fs::read_to_string(&output).unwrap(), "header\nx\ny\n");

    fs::remove_file(&input).ok();
    fs::remove_file(&output).ok();
}

/// Opening a missing source file surfaces an error at wiring time (with
/// context), rather than panicking.
#[test]
fn missing_source_file_is_an_error() {
    let g = GraphBuilder::new();
    let missing = temp_path("does_not_exist");
    // `.err()` drops the `Ok(Stream)` (which is not `Debug`), so this avoids the
    // `Debug` bound `unwrap_err` would require.
    let err = replay_lines(&g, &missing)
        .err()
        .expect("opening a missing file must error");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("lines adapter: opening source file"),
        "unexpected error: {msg}"
    );
}
