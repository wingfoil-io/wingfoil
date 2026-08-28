//! ws adapter — a reconnecting WebSocket client feeding a graph.
//!
//! Run with:
//!
//! ```sh
//! cargo run --example ws_adapter --features ws
//! ```
//!
//! The example is self-contained: it starts a small WebSocket server in-process
//! that publishes synthetic quotes, **deliberately drops the connection
//! part-way through**, and wires a graph against it with [`ws_connect`]. The
//! point is the drop — the graph keeps receiving quotes across it without any
//! reconnect handling of its own, and the `WsStatus` stream reports the
//! transition on-graph.
//!
//! A real venue adapter differs only in the URL, the subscription payload, and
//! a parsing stage that turns each frame into `market` types.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as TungsteniteMessage;
use wingfoil::adapters::ws::{WsBackoff, WsConfig, WsMessage, WsStatus, ws_connect};
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

/// Quotes each connection publishes before the server hangs up.
const QUOTES_PER_CONNECTION: usize = 3;

fn main() -> anyhow::Result<()> {
    let shutdown = Arc::new(AtomicBool::new(false));
    let port = start_venue(shutdown.clone())?;

    let g = GraphBuilder::new();
    let connection = ws_connect(
        &g,
        RunMode::RealTime,
        WsConfig::new(format!("ws://127.0.0.1:{port}/stream"))
            .subscribe(r#"{"op":"subscribe","args":["quotes.BTC-USD"]}"#)
            .backoff(WsBackoff {
                initial: Duration::from_millis(100),
                max: Duration::from_millis(500),
                ..WsBackoff::default()
            }),
    )?;

    // Status first, so the transition is visible before the frames that follow.
    let _status_sink = connection.status.for_each(|status: &WsStatus| {
        match status {
            WsStatus::Connected => println!("-- connected, subscription sent"),
            WsStatus::Reconnecting { attempt } => {
                println!("-- venue hung up; reconnecting (attempt {attempt})")
            }
            other => println!("-- {other:?}"),
        }
        Ok(())
    });

    let _quote_sink = connection.messages.for_each(|frames: &Burst<WsMessage>| {
        for frame in frames.iter() {
            if let Some(text) = frame.as_text() {
                println!("quote: {text}");
            }
        }
        Ok(())
    });

    g.build().run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_millis(1_200)),
    )?;

    shutdown.store(true, Ordering::Relaxed);
    println!("-- done");
    Ok(())
}

/// A synthetic venue: publishes a few quotes per connection, then hangs up.
///
/// Standing in for the real thing, which does the same at less convenient
/// moments.
fn start_venue(shutdown: Arc<AtomicBool>) -> anyhow::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let port = listener.local_addr()?.port();

    std::thread::Builder::new()
        .name("ws-example-venue".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("venue: building runtime: {e}");
                    return;
                }
            };
            runtime.block_on(async move {
                let Ok(listener) = TcpListener::from_std(listener) else {
                    eprintln!("venue: adopting listener failed");
                    return;
                };
                let sequence = Arc::new(AtomicUsize::new(0));
                while !shutdown.load(Ordering::Relaxed) {
                    let accept =
                        tokio::time::timeout(Duration::from_millis(50), listener.accept()).await;
                    let Ok(Ok((stream, _))) = accept else {
                        continue;
                    };
                    let sequence = sequence.clone();
                    tokio::spawn(async move {
                        serve_one(stream, sequence).await;
                    });
                }
            });
        })
        .map_err(|e| anyhow::anyhow!("spawning the venue thread: {e}"))?;

    Ok(port)
}

async fn serve_one(stream: tokio::net::TcpStream, sequence: Arc<AtomicUsize>) {
    let Ok(mut socket) = tokio_tungstenite::accept_async(stream).await else {
        return;
    };

    // Wait for the subscription before publishing, as a venue would.
    if let Ok(Some(Ok(TungsteniteMessage::Text(subscription)))) =
        tokio::time::timeout(Duration::from_millis(500), socket.next()).await
    {
        println!("venue: received {subscription}");
    }

    for _ in 0..QUOTES_PER_CONNECTION {
        let n = sequence.fetch_add(1, Ordering::Relaxed);
        let bid = 50_000.0 + n as f64;
        let quote = format!(
            r#"{{"symbol":"BTC-USD","bid":{bid:.1},"ask":{:.1}}}"#,
            bid + 0.5
        );
        if socket
            .send(TungsteniteMessage::Text(quote.into()))
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    // Hang up, so the example demonstrates a reconnect.
    let _ = socket.close(None).await;
}
