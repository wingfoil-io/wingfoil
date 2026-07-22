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
        // A distinct `step` per record yields one line per burst, a zero step
        // groups them; either way timestamps stay non-decreasing (the channel
        // receiver requires it). `try_from` turns the u32 tick-index ceiling
        // into a clear error rather than a silent wrap that would rewind the
        // clock and fail the run far from its cause.
        let tick = u32::try_from(i).with_context(|| {
            format!(
                "lines adapter: {} has more lines than the {} the replay clock can schedule",
                path.display(),
                u32::MAX
            )
        })?;
        sender.send_at(line, base + step * tick);
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
/// not yet terminated by a newline, or a read error, yields no tick. A line
/// that straddles a poll boundary (its bytes arrive across several polls) is
/// **reassembled**: the partial bytes are held until the terminating newline
/// arrives, then emitted whole — never in fragments and never dropped. This is
/// a deliberately minimal tail (it does not follow rotations) — the
/// deterministic path is [`replay_lines`]. Returns an error if the file cannot
/// be opened.
pub fn tail_lines(g: &GraphBuilder, path: impl AsRef<Path>) -> Result<Stream<Burst<String>>> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("lines adapter: opening tail file {}", path.display()))?;
    // `read_line` consumes whatever bytes are available even when no newline is
    // present, so a line whose bytes span multiple polls must be accumulated in
    // `pending` across calls; a fresh buffer each poll would drop those bytes
    // and later emit a corrupted remainder. `pending` carries them until the
    // newline lands.
    let state = Rc::new(RefCell::new((BufReader::new(file), String::new())));
    Ok(g.poll(move || {
        let (reader, pending) = &mut *state.borrow_mut();
        poll_line(reader, pending).map(|line| Burst::from([line]))
    }))
}

/// Read toward the next newline, accumulating partial bytes in `pending` across
/// calls. Returns the completed line (its trailing `\n`, and any `\r`, stripped)
/// once the terminator arrives, resetting `pending`; returns `None` on EOF, a
/// still-unterminated line, or a read error — leaving the bytes read so far in
/// `pending` for the next call. Factored out of [`tail_lines`] so the
/// straddled-line reassembly is unit-testable without a realtime run.
fn poll_line<R: BufRead>(reader: &mut R, pending: &mut String) -> Option<String> {
    match reader.read_line(pending) {
        // A complete, newline-terminated line: take it (stripping the line
        // ending) and clear `pending` for the next line.
        Ok(n) if n > 0 && pending.ends_with('\n') => {
            pending.pop();
            if pending.ends_with('\r') {
                pending.pop();
            }
            Some(std::mem::take(pending))
        }
        // EOF, a partial (un-terminated) trailing line, or a read error:
        // nothing to emit this cycle. Whatever was read stays in `pending`.
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::poll_line;
    use std::collections::VecDeque;
    use std::io::{BufReader, Read, Result as IoResult};

    /// A reader that hands out preset chunks one `read` at a time; an empty
    /// chunk yields `Ok(0)` (a momentary EOF), modelling a file still being
    /// appended to — exactly the straddled-line case [`poll_line`] must
    /// reassemble.
    struct ChunkReader {
        chunks: VecDeque<&'static [u8]>,
    }

    impl Read for ChunkReader {
        fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
            match self.chunks.pop_front() {
                Some(chunk) => {
                    let n = chunk.len().min(buf.len());
                    buf[..n].copy_from_slice(&chunk[..n]);
                    Ok(n)
                }
                None => Ok(0),
            }
        }
    }

    /// The regression this guards: `read_line` consumes the partial bytes even
    /// with no newline, so a fresh-buffer-per-poll tail would drop "par" and
    /// later emit a corrupted "tial". `poll_line` must hold the partial line and
    /// emit it whole once the terminator arrives.
    #[test]
    fn poll_line_reassembles_a_line_split_across_polls() {
        // "par" arrives, then a momentary EOF, then the rest "tial\n".
        let mut reader = BufReader::new(ChunkReader {
            chunks: VecDeque::from([&b"par"[..], &b""[..], &b"tial\n"[..]]),
        });
        let mut pending = String::new();

        // First poll: only the partial "par" — nothing emitted, bytes retained.
        assert_eq!(poll_line(&mut reader, &mut pending), None);
        assert_eq!(pending, "par");

        // Second poll: the rest lands and the whole line comes out intact.
        assert_eq!(
            poll_line(&mut reader, &mut pending),
            Some("partial".to_string())
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn poll_line_strips_crlf_and_emits_each_line_once() {
        let mut reader = BufReader::new(ChunkReader {
            chunks: VecDeque::from([&b"alpha\r\nbeta\n"[..]]),
        });
        let mut pending = String::new();
        assert_eq!(
            poll_line(&mut reader, &mut pending),
            Some("alpha".to_string())
        );
        assert_eq!(
            poll_line(&mut reader, &mut pending),
            Some("beta".to_string())
        );
        // Drained: no more complete lines.
        assert_eq!(poll_line(&mut reader, &mut pending), None);
        assert!(pending.is_empty());
    }
}
