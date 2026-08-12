//! A line-oriented file adapter — the smallest complete demonstration of an
//! Op-pattern I/O edge, in both directions.
//!
//! It reads and writes newline-delimited **records** (plain `String` lines, no
//! serde) using nothing but `std::fs` / `std::io`. It is the dependency-light
//! cousin of the [`csv`](crate::adapters::csv) adapter: the same shape, without
//! the parsing. The realtime [`tail_lines`] source and the file sink are
//! **dependency-free** (default build); the historical replay sources
//! ([`replay_lines`] / [`replay_lines_scheduled`]) sit behind the `async`
//! feature — see "Historical replay" below.
//!
//! # Deviations from legacy
//!
//! **None — there is no legacy `lines` adapter.** This module is new capability
//! in wingfoil, so nothing here can regress against the parity oracle and the
//! deviation register carries no row for it. This block exists so the
//! register's Source-(1) grep (see `docs/planning/deviation-register.md`) accounts for
//! every adapter module rather than silently skipping the ones with nothing to
//! say. Its closest legacy relative is the `csv` adapter, whose shape it
//! follows without the parsing.
//!
//! # Layering
//!
//! Following the [`stats`](crate::stats) module's pattern, the adapter is *not*
//! in the [`prelude`](crate::prelude). Bring in what you need explicitly:
//!
//! - **Sources** — free builder functions on a [`GraphBuilder`]:
//!   [`replay_lines`] / [`replay_lines_scheduled`] (deterministic historical
//!   replay, `async` feature) and [`tail_lines`] (a realtime `poll`-driven tail,
//!   dependency-free). Each returns a `Stream<Burst<String>>`.
//! - **Sink** — the [`LinesSinkOps`] extension trait on `Stream<Burst<T>>`,
//!   enabled with `use wingfoil::adapters::lines::LinesSinkOps;`. (A
//!   single-value `Stream<T>` maps first — see the trait docs for why there is
//!   no `Stream<T>` convenience impl here.)
//!
//! # Historical replay (the burst model)
//!
//! [`replay_lines`] opens the file at wiring (fail-fast) and streams its lines
//! **lazily** over a
//! [`produce_async`](crate::async_source::produce_async)
//! producer — exactly like [`csv_read`](crate::adapters::csv::csv_read): record
//! `i` is read on demand, stamped at `base + i * step`, and delivered on the
//! graph clock. The channel receiver groups same-timestamp records into one
//! atomic [`Burst`](crate::Burst) and replays them deterministically at their
//! timestamps — lossless, in order, independent of wall-clock. With the default
//! `step` of one nanosecond every record lands at a distinct instant, so each
//! burst carries exactly one line; a zero `step` (or a `base`/`step` that makes
//! records collide) groups colliding records into a single burst.
//!
//! `buffer_size` bounds the replay's look-ahead as back-pressure, so a huge file
//! is **not** read into memory up front — lines are pulled only as the graph
//! drains (a same-time group of any size rides one slot). This is why the replay
//! sources need the `async` feature (the lazy, bounded producer); the file I/O
//! itself is synchronous, on the producer task.
//!
//! # Sink
//!
//! [`LinesSinkOps::write_lines`] / [`LinesSinkOps::append_lines`] open the
//! target file once at wiring time and return a `Stream<()>` sink built on the
//! shared [`for_each_mut`](crate::fluent::StreamOps::for_each_mut) op: each
//! cycle writes every record in the incoming burst as its own line and flushes,
//! with any I/O error propagated through the fallible cycle (`?` + `.context`).

use std::cell::RefCell;
use std::fmt::Display;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::rc::Rc;

use anyhow::{Context, Result};

// Only the (async-gated) historical replay sources use the graph clock types and
// the `produce_async` ergonomic; the dependency-free tail + sink do not.
#[cfg(feature = "async")]
use std::time::Duration;
#[cfg(feature = "async")]
use wingfoil::NanoTime;

#[cfg(feature = "async")]
use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, SourceOps, Stream, StreamOps};
use crate::{Burst, burst};

/// Deterministic historical replay of a newline-delimited file, one record per
/// successive graph instant.
///
/// Sugar for [`replay_lines_scheduled`] with `base = NanoTime::ZERO` and
/// `step = 1ns`, so record `i` is delivered at time `i` and every burst holds
/// exactly one line. Run the graph with `RunMode::HistoricalFrom(t)` where
/// `t <= base` (e.g. `NanoTime::ZERO`).
///
/// Behind the `async` feature — see [`replay_lines_scheduled`] for why (lazy,
/// back-pressured replay via `produce_async`). `buffer_size` bounds the
/// replay's look-ahead as back-pressure (`None` = unbounded), exactly as
/// [`csv_read`](crate::adapters::csv::csv_read)'s `buffer_size`.
///
/// # Errors
///
/// Returns an error at wiring time if the file cannot be opened.
#[cfg(feature = "async")]
pub fn replay_lines(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<String>>> {
    replay_lines_scheduled(
        g,
        path,
        NanoTime::ZERO,
        Duration::from_nanos(1),
        buffer_size,
    )
}

/// Deterministic historical replay with an explicit schedule: record `i` is
/// stamped at `base + i * step` and replayed on the graph clock as a
/// [`Burst`]. Records sharing a timestamp (e.g. `step == 0`) ride one atomic
/// burst; otherwise each burst holds a single line.
///
/// The file is opened at wiring (fail-fast) but its lines are read **lazily** —
/// like [`csv_read`](crate::adapters::csv::csv_read), the replay rides a
/// [`produce_async`](crate::async_source::produce_async)
/// producer over a synchronous line iterator, so a huge file is never read into
/// memory up front. `base` must be at or after the run's start time (a pre-start
/// timestamp aborts the run). `buffer_size` bounds the look-ahead as
/// back-pressure (`None` = unbounded); a same-time group of any size rides one
/// slot, so it never deadlocks.
///
/// Behind the `async` feature (it uses the `produce_async` ergonomic); the
/// dependency-free [`tail_lines`] source and the file sink stay available in the
/// default build. The file I/O is synchronous on the producer task.
///
/// # Errors
///
/// Returns an error at wiring time if the file cannot be opened. A line that
/// cannot be read, or a line index beyond `u32::MAX`, aborts the run mid-stream
/// with context (surfaced as the reader reaches it).
#[cfg(feature = "async")]
pub fn replay_lines_scheduled(
    g: &GraphBuilder,
    path: impl AsRef<Path>,
    base: NanoTime,
    step: Duration,
    buffer_size: Option<usize>,
) -> Result<Stream<Burst<String>>> {
    let path = path.as_ref();
    let file = File::open(path)
        .with_context(|| format!("lines adapter: opening source file {}", path.display()))?;
    let display = path.display().to_string();
    // A lazy synchronous iterator: read a line, stamp it at `base + i * step`.
    // `BufReader::lines()` owns the file, so the iterator is `Send + 'static`; it
    // is pulled on demand by the producer (bounded by `buffer_size`), so the file
    // is never read into memory up front. A distinct `step` per record yields one
    // line per burst, a zero step groups them; either way timestamps stay
    // non-decreasing (the receiver requires it). `try_from` turns the u32
    // tick-index ceiling into a clear error rather than a silent wrap that would
    // rewind the clock.
    let rows = BufReader::new(file)
        .lines()
        .enumerate()
        .map(move |(i, line)| -> Result<(NanoTime, String)> {
            let line = line
                .with_context(|| format!("lines adapter: reading line {i} of {display}"))?;
            let tick = u32::try_from(i).with_context(|| {
                format!(
                    "lines adapter: {display} has more lines than the {} the replay clock can schedule",
                    u32::MAX
                )
            })?;
            Ok((base + step * tick, line))
        });
    produce_async(
        g,
        move |_p: RunParams| async move { Ok(futures::stream::iter(rows)) },
        buffer_size,
    )
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
/// deterministic path is [`replay_lines`].
///
/// # Errors
///
/// Returns an error if the file cannot be opened.
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
        poll_line(reader, pending).map(|line| burst![line])
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
/// [`for_each_mut`](crate::fluent::StreamOps::for_each_mut) op the same way
/// [`StatisticsOps`](crate::stats::StatisticsOps) layers over `wire`. Each
/// emitted burst writes every record as its own line, then flushes; an I/O
/// error aborts the run with context.
///
/// Unlike [`CsvSinkOps`](crate::adapters::csv::CsvSinkOps) and
/// [`EtcdSinkOps`](crate::adapters::etcd::EtcdSinkOps), this trait deliberately
/// has **no single-value `Stream<T>` convenience impl**: `Burst<T>` is itself
/// `Display` (a `tinyvec` renders as `[a, b]`), so a `Stream<Burst<T>>` *is* a
/// `Stream<T: Display>` — a second impl for `Stream<T>` would be ambiguous with
/// (or silently shadow) the burst form. A single-value stream therefore maps
/// first: `stream.map(|v| burst![v.clone()]).write_lines(path)`.
pub trait LinesSinkOps<T> {
    /// Write each record to `path` as a line, **truncating** the file first
    /// (created if absent). Returns the sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
    fn write_lines(&self, path: impl AsRef<Path>) -> Result<Stream<()>>;

    /// Write each record to `path` as a line, **appending** to any existing
    /// content (created if absent). Returns the sink `Stream<()>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened.
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
/// once (truncate for write, append for append) and wire a
/// [`for_each_mut`](crate::fluent::StreamOps::for_each_mut) sink that writes
/// every record in each burst as a line, flushing per cycle.
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
    Ok(
        stream.for_each_mut(BufWriter::new(file), move |w, burst: &Burst<T>| {
            for record in burst.iter() {
                writeln!(w, "{record}")
                    .with_context(|| format!("lines adapter: writing to {display}"))?;
            }
            w.flush()
                .with_context(|| format!("lines adapter: flushing {display}"))?;
            Ok(())
        }),
    )
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
