//! KDB+ real-time subscription via a tickerplant.
//!
//! [`kdb_sub`] is the real-time counterpart to [`kdb_read`](super::kdb_read)
//! (bounded historical replay): it subscribes to a q **tickerplant** and streams
//! rows on-graph as they are published, using the same [`KdbDeserialize`] impls.

use super::read::{KdbDeserialize, KdbExt, upd_payload_rows};
use super::{KdbConnection, SymbolInterner};
use crate::Burst;
use crate::async_source::{RunParams, produce_async};
use crate::fluent::{GraphBuilder, Stream};
use anyhow::{Context, Result};
use kdb_plus_fixed::ipc::{ConnectionMethod, QStream};
use kdb_plus_fixed::qtype;
use log::info;
use wingfoil_next::RunMode;

/// Live-subscribe to a KDB+ tickerplant and stream published rows in real time.
///
/// Complements [`kdb_read`](super::kdb_read) (bounded historical replay) with an
/// unbounded real-time source. Rows published to `table` while the graph runs are
/// streamed on-graph as `Burst<T>`, decoded by the same [`KdbDeserialize`] impl
/// `kdb_read` uses.
///
/// # How it works
///
/// A tickerplant is genuinely push-based (unlike the postgres `LISTEN`/`NOTIFY`
/// model, where the notification is only a wake-up and rows are re-queried):
///
/// 1. Opens a connection and sends `.u.sub[`table;syms]` **synchronously**. The
///    reply carries the table schema, from which the column names are captured.
/// 2. Loops on [`QStream::receive_message`], decoding each async
///    `(`upd; table; data)` message the tickerplant pushes. The `data` payload —
///    a table or a bare list of column vectors — is turned into rows and each is
///    handed to `T::from_kdb_row`. Non-`upd` control messages (e.g. the
///    end-of-day `.u.end`) are ignored.
///
/// # Subscription arguments
///
/// - `table` — the tickerplant table name (no leading backtick), e.g. `"trades"`.
/// - `symbols` — a q symbol-list literal selecting which syms to receive:
///   `` "`" `` for all, `` "`AAPL`MSFT" `` to filter. Spliced into the `.u.sub`
///   call, mirroring `kdb_read`'s caller-builds-the-query approach.
///
/// # Errors
///
/// Returns an error at **wiring time** if `run_mode` is
/// [`RunMode::HistoricalFrom`]: a tickerplant tail is a live, unbounded,
/// wall-clock-stamped stream with no historical timeline to replay, and the
/// historical channel path would block-collect it up front and deadlock at
/// `start`. Use [`kdb_read`](super::kdb_read) for historical replay and run
/// `kdb_sub` under [`RunMode::RealTime`]. A connection or subscription failure
/// (host:port only — never the password) aborts the run with context.
///
/// `.u.sub` tails from the moment of subscription — it does **not** replay the
/// tickerplant's log or the RDB's in-memory buffer, so rows published before the
/// graph started are not delivered. Under [`RunMode::RealTime`] the on-graph time
/// is the row's arrival (wall-clock) time; the timestamp column extracted by
/// `from_kdb_row` is carried through but does not order the real-time stream.
pub fn kdb_sub<T>(
    g: &GraphBuilder,
    run_mode: RunMode,
    connection: KdbConnection,
    table: impl Into<String>,
    symbols: impl Into<String>,
) -> Result<Stream<Burst<T>>>
where
    T: KdbDeserialize + Clone + Default + Send + 'static,
{
    if let RunMode::HistoricalFrom(_) = run_mode {
        anyhow::bail!(
            "kdb_sub: RunMode::HistoricalFrom is unsupported — the tickerplant tail is a \
             live, unbounded, wall-clock-stamped stream with no historical timeline to \
             replay; use kdb_read for historical replay and run kdb_sub under \
             RunMode::RealTime"
        );
    }
    let table = table.into();
    let symbols = symbols.into();

    produce_async(
        g,
        move |_p: RunParams| async move {
            let creds = connection.credentials_string();
            let mut socket = QStream::connect(
                ConnectionMethod::TCP,
                &connection.host,
                connection.port,
                &creds,
            )
            .await
            .with_context(|| format!("kdb_sub: failed to connect to {}", connection.redacted()))?;

            // Subscribe synchronously; the reply is `(`table; schema)`, from which we
            // capture the column names for positional row decoding.
            let sub_query = format!(".u.sub[`{table};{symbols}]");
            info!("kdb_sub: {sub_query}");
            let sub_reply = socket
                .send_sync_message(&sub_query.as_str())
                .await
                .with_context(|| format!("kdb_sub: subscription `{sub_query}` failed"))?;
            let columns: Vec<String> = sub_reply
                .element_at(1)
                .ok()
                .and_then(|schema| schema.column_names().ok())
                .unwrap_or_default();

            Ok(async_stream::stream! {
                let mut interner = SymbolInterner::default();
                loop {
                    let (_msg_type, msg) = match socket.receive_message().await {
                        Ok(m) => m,
                        Err(e) => {
                            yield Err(anyhow::Error::new(e).context("kdb_sub: receive failed"));
                            break;
                        }
                    };

                    // A tickerplant update is `(`upd; `table; data)` — a 3-element
                    // compound list whose first element is the symbol `upd`. Skip
                    // anything else (heartbeats, `.u.end`, etc.).
                    if msg.get_type() != qtype::COMPOUND_LIST || msg.len() < 3 {
                        continue;
                    }
                    match msg.element_at(0).ok().as_ref().and_then(|f| f.get_symbol().ok()) {
                        Some("upd") => {}
                        _ => continue,
                    }

                    let data = match msg.element_at(2) {
                        Ok(d) => d,
                        Err(e) => {
                            yield Err(anyhow::Error::new(e).context("kdb_sub: upd message has no data"));
                            break;
                        }
                    };

                    let rows = match upd_payload_rows(&data) {
                        Ok(rows) => rows,
                        Err(e) => { yield Err(e); break; }
                    };

                    for row in &rows {
                        match T::from_kdb_row(row, &columns, &mut interner) {
                            Ok((time, record)) => yield Ok((time, record)),
                            Err(e) => { yield Err(anyhow::Error::new(e)); return; }
                        }
                    }
                }
            })
        },
        None,
    )
}
