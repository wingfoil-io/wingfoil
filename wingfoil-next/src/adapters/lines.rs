//! A dependency-free, line-oriented file adapter — the smallest complete
//! demonstration of an Op-pattern I/O edge, in both directions.
//!
//! It reads and writes newline-delimited **records** (plain `String` lines, no
//! serde) using nothing but `std::fs` / `std::io`. It is the dependency-free
//! cousin of the planned CSV adapter (see `docs/port-plan.md`, Phase 4:
//! "csv — replay source + sink; exercises historical bursts"): the same shape,
//! without the parsing.
//!
//! # Layering
//!
//! Following the [`stats`](crate::stats) module's pattern, the adapter is *not*
//! in the [`prelude`](crate::prelude). Bring in what you need explicitly:
//!
//! - **Sources** — free builder functions on a [`GraphBuilder`]:
//!   [`replay_lines`] / [`replay_lines_scheduled`] (deterministic historical
//!   replay) and [`tail_lines`] (a realtime `poll`-driven tail). Each returns a
//!   `Stream<Burst<String>>`.
//! - **Sink** — the [`LinesSinkOps`] extension trait on
//!   `Stream<Burst<T>>`, enabled with
//!   `use wingfoil_next::adapters::lines::LinesSinkOps;`.
//!
//! # Historical replay (the burst model)
//!
//! [`replay_lines`] reads the whole file up front and schedules each record
//! onto the graph clock through a [`channel`](crate::fluent::SourceOps::channel)
//! source: record `i` is stamped at `base + i * step` with
//! [`ChannelSender::send_at`](crate::channel::ChannelSender::send_at), and the
//! sender is [`close`](crate::channel::ChannelSender::close)d immediately. The
//! channel receiver groups same-timestamp records into one atomic
//! [`Burst`](crate::Burst) and replays them deterministically at their
//! timestamps — lossless, in order, independent of wall-clock. With the default
//! `step` of one nanosecond every record lands at a distinct instant, so each
//! burst carries exactly one line; a zero `step` (or a `base`/`step` that makes
//! records collide) groups colliding records into a single burst. The whole
//! feed is queued *before* the run, so replay needs no producer thread and is
//! fully reproducible under `RunMode::HistoricalFrom`.
//!
//! # Sink
//!
//! [`LinesSinkOps::write_lines`] / [`LinesSinkOps::append_lines`] open the
//! target file once at wiring time and return a `Stream<()>` sink built on the
//! existing [`for_each`](crate::fluent::StreamOps::for_each) op: each cycle
//! writes every record in the incoming burst as its own line and flushes, with
//! any I/O error propagated through the fallible cycle (`?` + `.context`).

use std::cell::RefCell;
use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use anyhow::{Context, Result};
use wingfoil::NanoTime;

use crate::Burst;
use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};

/// Deterministic historical replay of a newline-delimited file, one record per
/// successive graph instant.
///
/// Sugar for [`replay_lines_scheduled`] with `base = NanoTime::ZERO` and
/// `step = 1ns`, so record `i` is delivered at time `i` and every burst holds
/// exactly one line. Run the graph with `RunMode::HistoricalFrom(t)` where
/// `t <= base` (e.g. `NanoTime::ZERO`).
///
/// Returns an error if the file cannot be opened or a line cannot be read.
pub fn replay_lines(g: &GraphBuilder, path: impl AsRef<Path>) -> Result<Stream<Burst<String>>> {
    replay_lines_scheduled(g, path, NanoTime::ZERO, Duration::from_nanos(1))
}

/// Deterministic historical replay with an explicit schedule: record `i` is
/// stamped at `base + i * step` and replayed on the graph clock as a
/// [`Burst`]. Records sharing a timestamp (e.g. `step == 0`) ride one atomic
/// burst; otherwise each burst holds a single line.
///
/// The file is read in full and queued onto a
/// [`channel`](crate::fluent::SourceOps::channel) source up front, so replay is
/// reproducible and needs no producer thread. `base` must be at or after the
/// run's start time (the channel receiver rejects a pre-start timestamp).
///
/// Returns an error if the file cannot be opened or a line cannot be read.
pub fn replay_lines_scheduled(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    base: NanoTime,
    step: Duration,
) -> Result<Stream<Burst<String>>> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("lines adapter: opening source file {}", path.display()))?;
    let (stream, sender) = g.channel::<String>();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line =
            line.with_context(|| format!("lines adapter: reading line {i} of {}", path.display()))?;
        // `step * i` on the u32 tick index keeps timestamps non-decreasing (the
        // channel receiver requires it); a distinct `step` per record yields
        // one line per burst, a zero step groups them.
        sender.send_at(line, base + step * (i as u32));
    }
    // Queue the end-of-stream so the historical receiver stops collecting.
    sender.close();
    Ok(stream)
}

/// A best-effort **realtime** tail of a newline-delimited file: a busy-poll
/// [`poll`](crate::fluent::SourceOps::poll) source that emits each newly read
/// line as a single-element [`Burst`]. Requires `RunMode::RealTime` (poll
/// sources have nothing to busy-poll in a historical replay).
///
/// Reads whatever the file holds at `poll` time, line by line; a line that is
/// not yet terminated by a newline, or a read error, yields no tick. This is a
/// deliberately minimal tail (it does not follow rotations) — the deterministic
/// path is [`replay_lines`]. Returns an error if the file cannot be opened.
pub fn tail_lines(g: &GraphBuilder, path: impl AsRef<Path>) -> Result<Stream<Burst<String>>> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("lines adapter: opening tail file {}", path.display()))?;
    let reader = Rc::new(RefCell::new(BufReader::new(file)));
    Ok(g.poll(move || {
        let mut buf = String::new();
        match reader.borrow_mut().read_line(&mut buf) {
            // A complete, newline-terminated line: emit it (without the `\n`).
            Ok(n) if n > 0 && buf.ends_with('\n') => {
                buf.pop();
                if buf.ends_with('\r') {
                    buf.pop();
                }
                Some(Burst::from([buf]))
            }
            // EOF, a partial (un-terminated) trailing line, or a read error:
            // nothing to emit this cycle.
            _ => None,
        }
    }))
}

/// A line-oriented file sink — the outbound counterpart of [`replay_lines`].
///
/// An extension trait on `Stream<Burst<T>>` (so `use`ing it enables
/// `stream.write_lines(path)` chaining), layered over the existing
/// [`for_each`](crate::fluent::StreamOps::for_each) op the same way
/// [`StatisticsOps`](crate::stats::StatisticsOps) layers over `wire`. Each
/// emitted burst writes every record as its own line, then flushes; an I/O
/// error aborts the run with context.
pub trait LinesSinkOps<T> {
    /// Write each record to `path` as a line, **truncating** the file first
    /// (created if absent). Returns the sink `Stream<()>`, or an error if the
    /// file cannot be opened.
    fn write_lines(&self, path: impl AsRef<Path>) -> Result<Stream<()>>;

    /// Write each record to `path` as a line, **appending** to any existing
    /// content (created if absent). Returns the sink `Stream<()>`, or an error
    /// if the file cannot be opened.
    fn append_lines(&self, path: impl AsRef<Path>) -> Result<Stream<()>>;
}

impl<T> LinesSinkOps<T> for Stream<Burst<T>>
where
    T: Display + Clone + Default + 'static,
{
    fn write_lines(&self, path: impl AsRef<Path>) -> Result<Stream<()>> {
        sink_lines(self, path, false)
    }

    fn append_lines(&self, path: impl AsRef<Path>) -> Result<Stream<()>> {
        sink_lines(self, path, true)
    }
}

/// Shared implementation of the two [`LinesSinkOps`] methods: open the file
/// once (truncate for write, append for append) and wire a `for_each` sink that
/// writes every record in each burst as a line, flushing per cycle.
fn sink_lines<T>(
    stream: &Stream<Burst<T>>,
    path: impl AsRef<Path>,
    append: bool,
) -> Result<Stream<()>>
where
    T: Display + Clone + Default + 'static,
{
    let path = path.as_ref();
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .with_context(|| format!("lines adapter: opening sink file {}", path.display()))?;
    let display = path.display().to_string();
    // `for_each` takes an `Fn`; the `BufWriter` needs `&mut`, so it lives behind
    // a `RefCell`.
    let writer = RefCell::new(BufWriter::new(file));
    Ok(stream.for_each(move |burst: &Burst<T>| {
        let mut w = writer.borrow_mut();
        for record in burst.iter() {
            writeln!(w, "{record}")
                .with_context(|| format!("lines adapter: writing to {display}"))?;
        }
        w.flush()
            .with_context(|| format!("lines adapter: flushing {display}"))?;
        Ok(())
    }))
}
