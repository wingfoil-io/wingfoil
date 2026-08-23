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
//! cargo test -p wingfoil --features fix-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
#![cfg(feature = "fix-integration-test")]

use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use wingfoil::adapters::fix::{
    FixOptions, FixPollMode, FixSeqNumStore, FixSessionStatus, fix_accept, fix_accept_with_options,
    fix_connect, fix_connect_with_options,
};
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

/// A clean logon exchange must raise **no** `SequenceGap` and no `Error` on
/// either side.
///
/// The point is the absence: sequence validation now runs on every inbound
/// message, so a session that is actually in sequence has to stay silent about
/// it. A validator that is too eager shows up here as a spurious gap or a
/// Reject-driven disconnect, which is the way this class of change usually
/// breaks.
#[test]
fn a_healthy_session_reports_no_sequence_problems() {
    let _ = env_logger::try_init();
    let port = free_port();
    let g = GraphBuilder::new();

    let (acc_data, acc_status) = fix_accept(
        &g,
        RunMode::RealTime,
        port,
        "ACCEPTOR",
        "INITIATOR",
        FixPollMode::Threaded,
    )
    .unwrap();
    let (init_data, init_status) = fix_connect(
        &g,
        RunMode::RealTime,
        "127.0.0.1",
        port,
        "INITIATOR",
        "ACCEPTOR",
        FixPollMode::Threaded,
    )
    .unwrap();

    let acc_seen = acc_status.collapse_accumulate();
    let init_seen = init_status.collapse_accumulate();
    let _acc_data = acc_data.collapse_accumulate();
    let _init_data = init_data.collapse_accumulate();

    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))
        .unwrap();

    for (who, seen) in [
        ("acceptor", runner.value(&acc_seen)),
        ("initiator", runner.value(&init_seen)),
    ] {
        let seen: Vec<FixSessionStatus> = seen;
        assert!(
            seen.contains(&FixSessionStatus::LoggedIn),
            "{who}: expected LoggedIn, got {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|s| matches!(s, FixSessionStatus::SequenceGap { .. })),
            "{who}: an in-sequence session must not report a gap: {seen:?}"
        );
        assert!(
            !seen.iter().any(|s| matches!(s, FixSessionStatus::Error(_))),
            "{who}: unexpected session error: {seen:?}"
        );
    }
}

/// A `FixSeqNumStore::File` session writes its sequence numbers, and they are
/// readable after the run — the on-disk half of restart continuity, over a real
/// socket rather than a `Vec<u8>`.
#[test]
fn a_file_backed_session_persists_its_sequence_numbers() {
    let _ = env_logger::try_init();
    let port = free_port();
    let dir = std::env::temp_dir();
    let stamp = format!("{}-{port}", std::process::id());
    let acc_path = dir.join(format!("wingfoil-fix-it-acc-{stamp}.txt"));
    let init_path = dir.join(format!("wingfoil-fix-it-init-{stamp}.txt"));
    let _ = std::fs::remove_file(&acc_path);
    let _ = std::fs::remove_file(&init_path);

    let g = GraphBuilder::new();
    let (acc_data, acc_status) = fix_accept_with_options(
        &g,
        RunMode::RealTime,
        port,
        "ACCEPTOR",
        "INITIATOR",
        FixPollMode::Threaded,
        FixOptions::default().with_seq_num_store(FixSeqNumStore::File(acc_path.clone())),
    )
    .unwrap();
    let (init_data, init_status) = fix_connect_with_options(
        &g,
        RunMode::RealTime,
        "127.0.0.1",
        port,
        "INITIATOR",
        "ACCEPTOR",
        FixPollMode::Threaded,
        FixOptions::default().with_seq_num_store(FixSeqNumStore::File(init_path.clone())),
    )
    .unwrap();

    let acc_seen = acc_status.collapse_accumulate();
    let _init_seen = init_status.collapse_accumulate();
    let _acc_data = acc_data.collapse_accumulate();
    let _init_data = init_data.collapse_accumulate();

    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))
        .unwrap();

    let acc: Vec<FixSessionStatus> = runner.value(&acc_seen);
    assert!(
        acc.contains(&FixSessionStatus::LoggedIn),
        "acceptor: expected LoggedIn, got {acc:?}"
    );

    // Both stores hold a `<outbound> <next inbound>` pair, and the session got
    // far enough to advance past FIX's starting point.
    for (who, path) in [("acceptor", &acc_path), ("initiator", &init_path)] {
        let text = std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("{who} store {} unreadable: {e}", path.display()));
        let nums: Vec<u64> = text
            .split_whitespace()
            .map(|s| s.parse().expect("store holds decimal numbers"))
            .collect();
        assert_eq!(
            nums.len(),
            2,
            "{who} store should hold two numbers: {text:?}"
        );
        assert!(
            nums[0] >= 1,
            "{who} should have sent at least a Logon, store says {nums:?}"
        );
        assert!(
            nums[1] >= 2,
            "{who} should have consumed at least one inbound message, store says {nums:?}"
        );
    }

    let _ = std::fs::remove_file(&acc_path);
    let _ = std::fs::remove_file(&init_path);
}
