//! Same-process, real-loopback-socket parity tests for the FIX adapter — a port
//! of the legacy `legacy/wingfoil/src/adapters/fix/mod.rs` unit tests that stand up an
//! in-process acceptor + initiator (`fix_same_process_spin`,
//! `fix_same_process_threaded`, `fix_connection_refused`,
//! `initiator_reconnects_after_a_session_drop`). No external service or
//! container — just loopback TCP — so they are gated behind
//! `fix-integration-test` (they run against a live wall clock and are timing
//! sensitive; the legacy credentialed LMAX-demo tests are **not** ported, as
//! they need external credentials).
//!
//! Because these run realtime, they assert received *values* (that both sides
//! reach `LoggedIn`, that a refused connect surfaces an `Error`, that a dropped
//! session reconnects) rather than exact tick times — a live session has no
//! historical timeline to replay deterministically. Run with:
//! ```sh
//! cargo test --manifest-path crates/wingfoil/Cargo.toml --features fix-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
#![cfg(feature = "fix-integration-test")]

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use wingfoil::adapters::fix::{FixPollMode, FixSessionStatus, fix_accept, fix_connect};
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

/// Allocate an ephemeral port by binding to :0 and immediately dropping the listener.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Stand up an acceptor and an initiator in **one** graph and assert both sides
/// reach `LoggedIn`. Parameterised over the poll mode so `AlwaysSpin` and
/// `Threaded` share the body — the legacy `fix_same_process_{spin,threaded}`
/// pair.
///
/// The acceptor is wired **first** so its listener binds (at graph `start()`)
/// before the initiator's synchronous `AlwaysSpin` connect runs — start hooks
/// fire in wiring order, and the spin initiator connects in its start hook.
fn same_process(make_mode: impl Fn() -> FixPollMode) {
    let _ = env_logger::try_init();
    let port = free_port();
    let g = GraphBuilder::new();

    let (acc_data, acc_status) = fix_accept(
        &g,
        RunMode::RealTime,
        port,
        "ACCEPTOR",
        "INITIATOR",
        make_mode(),
    )
    .unwrap();
    let (init_data, init_status) = fix_connect(
        &g,
        RunMode::RealTime,
        "127.0.0.1",
        port,
        "INITIATOR",
        "ACCEPTOR",
        make_mode(),
    )
    .unwrap();

    let acc_seen = acc_status.collapse_accumulate();
    let init_seen = init_status.collapse_accumulate();
    // Keep the data streams live in the graph even though we only assert status.
    let _acc_data = acc_data.collapse_accumulate();
    let _init_data = init_data.collapse_accumulate();

    let mut runner = g.build();
    runner
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(500)),
        )
        .unwrap();

    let acc: Vec<FixSessionStatus> = runner.value(&acc_seen);
    let init: Vec<FixSessionStatus> = runner.value(&init_seen);
    assert!(
        acc.contains(&FixSessionStatus::LoggedIn),
        "acceptor: expected LoggedIn, got: {acc:?}"
    );
    assert!(
        init.contains(&FixSessionStatus::LoggedIn),
        "initiator: expected LoggedIn, got: {init:?}"
    );
}

#[test]
fn fix_same_process_spin() {
    same_process(|| FixPollMode::AlwaysSpin);
}

#[test]
fn fix_same_process_threaded() {
    same_process(|| FixPollMode::Threaded);
}

/// An initiator connecting to a dead port surfaces an `Error` status (the
/// `Threaded` mode's connect-failure path) rather than panicking. Ports legacy's
/// `fix_connection_refused`.
#[test]
fn fix_connection_refused() {
    let g = GraphBuilder::new();
    // Port 1 is privileged and unbound — connect is refused.
    let (data, status) = fix_connect(
        &g,
        RunMode::RealTime,
        "127.0.0.1",
        1,
        "SENDER",
        "TARGET",
        FixPollMode::Threaded,
    )
    .unwrap();
    let statuses = status.collapse_accumulate();
    let _data = data.collapse_accumulate();

    let mut runner = g.build();
    // Generous duration so this doesn't flake under load — the Error status
    // usually arrives within tens of ms.
    runner
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(5)))
        .unwrap();

    let statuses: Vec<FixSessionStatus> = runner.value(&statuses);
    assert!(
        statuses
            .iter()
            .any(|s| matches!(s, FixSessionStatus::Error(_))),
        "expected an Error status from connection refusal, got: {statuses:?}"
    );
}

/// An initiator whose *established* session drops must reconnect (a dropped
/// session used to kill the feed permanently). A mock server that accepts a
/// connection then immediately closes it drives the initiator through connect →
/// session → EOF → reconnect; the test asserts the server is connected to more
/// than once in the window. Ports legacy's
/// `initiator_reconnects_after_a_session_drop`.
#[test]
fn initiator_reconnects_after_a_session_drop() {
    let _ = env_logger::try_init();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();

    let accepts = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let accepts_srv = accepts.clone();
    let stop_srv = stop.clone();
    // Accept then immediately drop each connection: the initiator connects,
    // starts the session, sees EOF, and — with reconnect — connects again.
    let server = std::thread::spawn(move || {
        while !stop_srv.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((sock, _)) => {
                    accepts_srv.fetch_add(1, Ordering::Relaxed);
                    drop(sock);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });

    let g = GraphBuilder::new();
    let (data, status) = fix_connect(
        &g,
        RunMode::RealTime,
        "127.0.0.1",
        port,
        "INIT",
        "ACC",
        FixPollMode::Threaded,
    )
    .unwrap();
    let _status = status.collapse_accumulate();
    let _data = data.collapse_accumulate();

    g.build()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))
        .unwrap();

    stop.store(true, Ordering::Relaxed);
    server.join().unwrap();
    let n = accepts.load(Ordering::Relaxed);
    assert!(
        n >= 2,
        "an initiator must reconnect after a session drop; server saw {n} connection(s)"
    );
}
