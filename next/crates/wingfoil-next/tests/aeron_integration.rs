//! Integration tests for the aeron adapter — a parity port of the classic
//! `wingfoil/src/adapters/aeron/integration_tests.rs`.
//!
//! Requires a running Aeron media driver via Docker. Run with:
//! ```sh
//! cargo test -p wingfoil-next --features aeron-integration-test \
//!   -- --test-threads=1 --nocapture
//! ```
//!
//! Image: `neomantra/aeron-cpp-debian` — ships `/usr/local/bin/aeronmd` as its
//! entrypoint. The driver writes its CNC file under `/dev/shm`; we bind-mount
//! `/dev/shm` so the host process connects to the same driver via `aeron:ipc`.
#![cfg(feature = "aeron-integration-test")]

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use testcontainers::{
    GenericImage, ImageExt,
    core::{Mount, WaitFor},
    runners::SyncRunner,
};
use wingfoil::{RunFor, RunMode};
use wingfoil_next::adapters::aeron::transport::{AeronPublisherBackend, AeronSubscriberBackend};
use wingfoil_next::adapters::aeron::{
    AeronHandle, AeronMode, AeronRsHandle, AeronSinkOps, AeronStatus, AeronSubOptions, ChannelUri,
    FragmentBuffer, TransportError, aeron_sub_fragment, aeron_sub_fragment_with_status,
};
use wingfoil_next::prelude::*;

/// `neomantra/aeron-cpp-debian` ships `aeronmd` as its entrypoint and logs
/// nothing to stdout/stderr, so we use a fixed-time `WaitFor` then poll for the
/// CNC file.
const DRIVER_IMAGE: &str = "neomantra/aeron-cpp-debian";
const DRIVER_TAG: &str = "latest";
/// IPC channel: communicates via the shared CNC file in /dev/shm — no UDP.
const AERON_CHANNEL: &str = "aeron:ipc";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Shared Aeron directory used by both the container's aeronmd and the host
/// client. Setting the same `AERON_DIR` on both sides ensures the CNC file path
/// matches regardless of which user aeronmd runs as inside the container.
const AERON_DIR: &str = "/dev/shm/aeron-next-integration-test";

/// Serialise the whole suite.
///
/// These tests are inherently non-parallel: the media driver writes its CNC file
/// to a fixed `AERON_DIR` under a bind-mounted `/dev/shm`, and the Aeron *client*
/// is pointed at it through a process-global environment variable that
/// `start_media_driver` (and `no_driver_connection_fails`) rewrite. Two tests in
/// flight at once would clobber each other's driver directory and env var.
///
/// The dedicated workflow passes `--test-threads=1`, but the main
/// `cargo test -p wingfoil-next --all-features` job does not — so the constraint
/// is enforced here rather than left to the caller. Every test takes this guard
/// first. The mutex is deliberately poison-tolerant: one failing test must not
/// cascade into "poisoned" failures for all the others.
fn serial_guard() -> MutexGuard<'static, ()> {
    static SERIAL: Mutex<()> = Mutex::new(());
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Start an Aeron media driver container and return a guard that stops it on
/// drop. Binds `/dev/shm` into the container so the host process shares the CNC
/// file.
fn start_media_driver() -> anyhow::Result<impl Drop> {
    // Tell the host Aeron client where to find the CNC file.
    // SAFETY: tests run with --test-threads=1, so no concurrent env access.
    unsafe { std::env::set_var("AERON_DIR", AERON_DIR) };

    // Remove any stale CNC files left by a previous run so the new aeronmd
    // creates a fresh one with a valid heartbeat.
    let _ = std::fs::remove_dir_all(AERON_DIR);

    // Run as the current user so the CNC file is writable by the test process:
    // Aeron clients open it O_RDWR, and a root-owned 0644 file would EACCES.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let container = GenericImage::new(DRIVER_IMAGE, DRIVER_TAG)
        // Minimal structural wait — we poll for the CNC file below.
        .with_wait_for(WaitFor::seconds(1))
        // Bind-mount the host's /dev/shm so aeronmd's CNC file is visible to the
        // host process. Do NOT call with_shm_size — that creates a separate
        // tmpfs which overrides the bind mount.
        .with_mount(Mount::bind_mount("/dev/shm", "/dev/shm"))
        .with_env_var("AERON_DIR", AERON_DIR)
        .with_user(format!("{uid}:{gid}"))
        .start()?;

    // Poll until aeronmd creates the CNC file (max 10 s).
    let cnc = std::path::Path::new(AERON_DIR).join("cnc.dat");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !cnc.exists() {
        anyhow::ensure!(
            Instant::now() < deadline,
            "timed out waiting for aeronmd CNC file: {}",
            cnc.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // Give aeronmd a moment to finish initialising the CNC file contents after
    // it first appears on disk.
    std::thread::sleep(Duration::from_millis(500));

    Ok(container)
}

/// Little-endian `i64` parser for the fragment surface.
fn i64_parser(frag: &FragmentBuffer<'_>) -> Result<Option<i64>, TransportError> {
    Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
}

/// Poll until the publication reports a connected subscriber image, or fail
/// after `timeout`. An Aeron publication is only "connected" once a matching
/// subscription has joined and the driver conductor has established the image;
/// until then `try_claim`/`offer` return `Connection("Not connected")`. Acting
/// on a publication before the image is ready is the dominant source of
/// flakiness on slower CI runners.
fn wait_for_pub_connected<P: AeronPublisherBackend>(
    publication: &P,
    timeout: Duration,
) -> anyhow::Result<()> {
    let deadline = Instant::now() + timeout;
    while !publication.is_connected() {
        anyhow::ensure!(
            Instant::now() < deadline,
            "publication did not connect within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Handle / backend behaviour
// ---------------------------------------------------------------------------

/// Without a running media driver, `AeronHandle::connect()` must return an
/// error. `AERON_DIR` points at an empty temp directory so a CNC file left by
/// an earlier test does not cause a spurious success.
#[test]
fn no_driver_connection_fails() {
    let _serial = serial_guard();
    const NO_DRIVER_DIR: &str = "/tmp/aeron-next-no-driver-test";
    let _ = std::fs::remove_dir_all(NO_DRIVER_DIR);
    // SAFETY: tests run with --test-threads=1, so no concurrent env access.
    unsafe { std::env::set_var("AERON_DIR", NO_DRIVER_DIR) };
    assert!(
        AeronHandle::connect().is_err(),
        "connect must fail with no media driver running"
    );
}

/// `try_claim` zero-copy round-trip: claim a 64-byte slot, fill it with `0xAB`,
/// commit, and assert the subscriber receives exactly those bytes.
#[test]
fn try_claim_zero_copy_roundtrip() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2005i32;

    let mut sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let mut publication = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    wait_for_pub_connected(&publication, CONNECT_TIMEOUT)?;

    let mut claim = publication.try_claim(64).expect("try_claim succeeds");
    assert_eq!(claim.len(), 64);
    for b in claim.data().iter_mut() {
        *b = 0xAB;
    }
    claim.commit().expect("commit succeeds");

    let mut received: Option<Vec<u8>> = None;
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && received.is_none() {
        let _ = sub.poll(&mut |bytes: &[u8]| received = Some(bytes.to_vec()));
        if received.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let bytes = received.expect("subscription observes the committed claim within 2s");
    assert_eq!(bytes.len(), 64);
    assert!(bytes.iter().all(|b| *b == 0xAB), "all 64 bytes are 0xAB");
    Ok(())
}

/// `try_claim` returns `TransportError::BackPressure` once the term buffer
/// saturates with no subscriber polling. Uses the minimum supported term length
/// so the buffer fills within a bounded iteration count.
#[test]
fn try_claim_back_pressure_returns_typed_error() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2006i32;
    let channel = "aeron:ipc?term-length=65536";

    // Hold a subscription (never polled, so the term buffer saturates) and wait
    // for the image before claiming, otherwise the first claim is "Not connected".
    let _sub = handle.subscription(channel, stream_id, CONNECT_TIMEOUT)?;
    let mut publication = handle.publication(channel, stream_id, CONNECT_TIMEOUT)?;
    wait_for_pub_connected(&publication, CONNECT_TIMEOUT)?;

    let mut back_pressure_seen = false;
    for _ in 0..200 {
        match publication.try_claim(1024) {
            Ok(claim) => claim.commit().expect("commit succeeds"),
            Err(TransportError::BackPressure) => {
                back_pressure_seen = true;
                break;
            }
            Err(other) => panic!("unexpected error from try_claim: {other:?}"),
        }
    }
    assert!(
        back_pressure_seen,
        "back-pressure never observed within 200 iterations on term-length=65536"
    );
    Ok(())
}

/// Dropping an un-committed `ClaimBuffer` releases the term-buffer slot via the
/// `Drop` backstop, so the next `try_claim` succeeds immediately rather than
/// waiting 15 s for the publication-unblock timeout.
#[test]
fn try_claim_drop_without_commit_aborts() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2007i32;

    let _sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let mut publication = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    wait_for_pub_connected(&publication, CONNECT_TIMEOUT)?;

    drop(publication.try_claim(64).expect("first try_claim succeeds"));
    publication
        .try_claim(64)
        .expect("second try_claim succeeds")
        .commit()
        .expect("commit succeeds");
    Ok(())
}

/// `fragment_limit` is a per-`poll()` cap, not a target.
///
/// Publish 1000 small messages, then poll with `fragment_limit = 32` and assert
/// every non-zero poll returns at most 32 fragments while all 1000 are delivered
/// in total. A buggy `fragment_limit` wiring would deliver all 1000 in one poll.
#[test]
fn fragment_limit_caps_burst_size() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2008i32;

    let mut sub = handle
        .subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?
        .with_fragment_limit(32);
    let mut publication = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let mut published = 0usize;
    let pub_deadline = Instant::now() + Duration::from_secs(10);
    while published < 1000 {
        anyhow::ensure!(
            Instant::now() < pub_deadline,
            "timed out publishing at {published} of 1000"
        );
        let bytes = (published as i64).to_le_bytes();
        match publication.offer(&bytes) {
            Ok(()) => published += 1,
            Err(_) => std::thread::sleep(Duration::from_micros(100)),
        }
    }

    let mut total = 0usize;
    let mut saw_capped_burst = false;
    let deadline = Instant::now() + Duration::from_secs(10);
    while total < 1000 {
        anyhow::ensure!(Instant::now() < deadline, "timed out at {total} of 1000");
        let count = sub.poll(&mut |_fragment: &[u8]| ())?;
        assert!(
            count <= 32,
            "fragment_limit=32 cap violated: poll returned {count}"
        );
        if count > 0 {
            saw_capped_burst = true;
        }
        total += count;
    }
    assert_eq!(total, 1000, "expected exactly 1000 fragments");
    assert!(
        saw_capped_burst,
        "expected at least one non-zero poll within the 32-fragment cap"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Source / sink round trips over a live driver
// ---------------------------------------------------------------------------

/// Publish `per_tick` values per tick and collect whatever the subscriber sees
/// within `secs`. Returns the flattened values.
fn round_trip<B, P>(
    sub: B,
    publication: P,
    opts: AeronSubOptions,
    per_tick: usize,
    secs: u64,
) -> anyhow::Result<Vec<i64>>
where
    B: AeronSubscriberBackend,
    P: AeronPublisherBackend,
{
    let g = GraphBuilder::new();
    let collected =
        aeron_sub_fragment(&g, RunMode::RealTime, sub, i64_parser, opts)?.collapse_accumulate();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(move |n: &u64| {
            (0..per_tick)
                .map(|i| *n as i64 * per_tick as i64 + i as i64)
                .collect::<Burst<i64>>()
        })
        .aeron_pub(publication, |v: &i64| v.to_le_bytes().to_vec());

    let mut runner = g.build();
    // A transport hiccup on teardown is not the subject of these tests (classic
    // ignores it too — the assertion is on what arrived).
    let _ = runner.run(
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(secs)),
    );
    Ok(runner.value(&collected))
}

/// Spin-mode typed-parser round-trip over the media driver.
#[test]
fn spin_sub_single_message_roundtrip() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2009i32;
    let values = round_trip(
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        AeronSubOptions::default(),
        1,
        2,
    )?;
    assert!(!values.is_empty(), "expected values, got: {values:?}");
    Ok(())
}

/// Spin-mode burst accumulation: publish bursts of 15 each tick; assert at least
/// 10 arrive in total.
#[test]
fn spin_sub_burst_accumulation() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2010i32;
    let values = round_trip(
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        AeronSubOptions {
            mode: AeronMode::Spin,
            fragment_limit: 256,
        },
        15,
        2,
    )?;
    assert!(
        values.len() >= 10,
        "expected at least 10 accumulated values, got {}",
        values.len()
    );
    Ok(())
}

/// Threaded-mode delivery across the background channel.
#[test]
fn threaded_sub_accumulates_across_channel_drain() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2011i32;
    let values = round_trip(
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        AeronSubOptions {
            mode: AeronMode::Threaded,
            fragment_limit: 256,
        },
        1,
        2,
    )?;
    assert!(
        !values.is_empty(),
        "expected threaded delivery, got: {values:?}"
    );
    Ok(())
}

/// `collapse` yields the latest value of each burst.
#[test]
fn collapse_yields_latest_value() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2012i32;

    let g = GraphBuilder::new();
    let latest = aeron_sub_fragment(
        &g,
        RunMode::RealTime,
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        i64_parser,
        AeronSubOptions::default(),
    )?
    .collapse()
    .accumulate();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(|n: &u64| burst![*n as i64 * 3, *n as i64 * 3 + 1, *n as i64 * 3 + 2])
        .aeron_pub(
            handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
            |v: &i64| v.to_le_bytes().to_vec(),
        );

    let mut runner = g.build();
    let _ = runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)));
    assert!(
        !runner.value(&latest).is_empty(),
        "expected collapsed values"
    );
    Ok(())
}

/// The typed parser sees a real (non-zero) fragment position from the rusteron
/// backend, which overrides `poll_fragments`.
#[test]
fn parser_sees_fragment_header_with_real_position() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2013i32;

    let g = GraphBuilder::new();
    let positions = aeron_sub_fragment(
        &g,
        RunMode::RealTime,
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
            Ok(Some(frag.position()))
        },
        AeronSubOptions::default(),
    )?
    .collapse_accumulate();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(|n: &u64| burst![*n as i64])
        .aeron_pub(
            handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
            |v: &i64| v.to_le_bytes().to_vec(),
        );

    let mut runner = g.build();
    let _ = runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)));
    let positions = runner.value(&positions);
    assert!(!positions.is_empty(), "expected at least one fragment");
    assert!(
        positions.iter().any(|p| *p > 0),
        "the rusteron backend must surface a real stream position, got {positions:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Status side-channels over a live driver
// ---------------------------------------------------------------------------

/// The subscriber's status stream emits `Connected` once a publisher joins, and
/// only once (transition-only).
#[test]
fn status_stream_emits_on_connect() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2014i32;

    let g = GraphBuilder::new();
    let (_data, status) = aeron_sub_fragment_with_status(
        &g,
        RunMode::RealTime,
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        i64_parser,
        AeronSubOptions::default(),
    )?;
    let collected = status.collapse_accumulate();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(|n: &u64| burst![*n as i64])
        .aeron_pub(
            handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
            |v: &i64| v.to_le_bytes().to_vec(),
        );

    let mut runner = g.build();
    let _ = runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)));
    let statuses = runner.value(&collected);
    assert!(
        statuses.contains(&AeronStatus::Connected),
        "expected a Connected transition, got {statuses:?}"
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == AeronStatus::Connected)
            .count(),
        1,
        "steady state must not re-emit: {statuses:?}"
    );
    Ok(())
}

/// Threaded mode propagates the same `Connected` transition, sampled at the poll
/// thread's cadence and carried in-band with the data.
#[test]
fn threaded_status_stream_emits_on_connect() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2015i32;

    let g = GraphBuilder::new();
    let (_data, status) = aeron_sub_fragment_with_status(
        &g,
        RunMode::RealTime,
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        i64_parser,
        AeronSubOptions {
            mode: AeronMode::Threaded,
            fragment_limit: 256,
        },
    )?;
    let collected = status.collapse_accumulate();
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(|n: &u64| burst![*n as i64])
        .aeron_pub(
            handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
            |v: &i64| v.to_le_bytes().to_vec(),
        );

    let mut runner = g.build();
    let _ = runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)));
    let statuses = runner.value(&collected);
    assert!(
        statuses.contains(&AeronStatus::Connected),
        "expected a Connected transition in threaded mode, got {statuses:?}"
    );
    Ok(())
}

/// The publisher's status stream reports `BackPressured` when the term buffer
/// saturates behind an unpolled subscriber.
#[test]
fn publisher_status_emits_back_pressure() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2016i32;
    let channel = "aeron:ipc?term-length=65536";

    // Hold an unpolled subscription so the term buffer fills.
    let publication = handle.publication(channel, stream_id, CONNECT_TIMEOUT)?;
    let _sub = handle.subscription(channel, stream_id, CONNECT_TIMEOUT)?;
    wait_for_pub_connected(&publication, CONNECT_TIMEOUT)?;

    let g = GraphBuilder::new();
    let (_sink, status) = g
        .ticker(Duration::from_micros(100))
        .count()
        .map(|n: &u64| (0..64).map(|i| *n as i64 * 64 + i).collect::<Burst<i64>>())
        .aeron_pub_with_status(publication, |v: &i64| vec![(*v % 251) as u8; 1024]);
    let collected = status.collapse_accumulate();

    let mut runner = g.build();
    let _ = runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(5)));
    let statuses = runner.value(&collected);
    assert!(
        statuses.contains(&AeronStatus::BackPressured),
        "expected a BackPressured transition, got {statuses:?}"
    );
    Ok(())
}

/// Every item of every burst is published — no dedup.
#[test]
fn publisher_publishes_every_burst_item() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2017i32;

    let g = GraphBuilder::new();
    let collected = aeron_sub_fragment(
        &g,
        RunMode::RealTime,
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        i64_parser,
        AeronSubOptions::default(),
    )?
    .collapse_accumulate();
    // The same value three times per tick: a deduping publisher would send one.
    let _sink = g
        .ticker(Duration::from_millis(10))
        .count()
        .map(|_: &u64| burst![7i64, 7, 7])
        .aeron_pub(
            handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
            |v: &i64| v.to_le_bytes().to_vec(),
        );

    let mut runner = g.build();
    let _ = runner.run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)));
    let values = runner.value(&collected);
    assert!(
        values.len() >= 3,
        "every burst item must be published, got {} values",
        values.len()
    );
    assert!(values.iter().all(|v| *v == 7));
    Ok(())
}

// ---------------------------------------------------------------------------
// Channel URIs
// ---------------------------------------------------------------------------

/// A round trip over the `ChannelUri::ipc()` builder's output.
#[test]
fn channel_uri_ipc_roundtrip() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2018i32;
    let channel = ChannelUri::ipc();

    let values = round_trip(
        handle.subscription(&channel, stream_id, CONNECT_TIMEOUT)?,
        handle.publication(&channel, stream_id, CONNECT_TIMEOUT)?,
        AeronSubOptions::default(),
        1,
        2,
    )?;
    assert!(!values.is_empty(), "expected values over aeron:ipc");
    Ok(())
}

/// A round trip over the MDC (multi-destination-cast) URI builders.
#[test]
fn channel_uri_mdc_roundtrip() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronHandle::connect()?;
    let stream_id = 2019i32;
    let control = "127.0.0.1:40456";
    let pub_channel = ChannelUri::mdc_publication(control)?;
    let sub_channel = ChannelUri::mdc_subscription("127.0.0.1:40457", control)?;

    // MDC joins over UDP loopback via a dynamic control handshake, which is much
    // slower to establish than IPC or plain unicast. The subscription must exist
    // before the publication can connect (it is the subscriber's status message
    // to the control address that adds the destination), so create it first and
    // only then wait for the image. Without this wait the graph starts offering
    // into an unconnected publication and the run ends before the join
    // completes, so nothing arrives. Classic's twin
    // (`test_channel_uri_mdc_roundtrip`) waits here for the same reason.
    let subscription = handle.subscription(&sub_channel, stream_id, CONNECT_TIMEOUT)?;
    let publication = handle.publication(&pub_channel, stream_id, CONNECT_TIMEOUT)?;
    wait_for_pub_connected(&publication, CONNECT_TIMEOUT)?;

    let values = round_trip(subscription, publication, AeronSubOptions::default(), 1, 3)?;
    assert!(!values.is_empty(), "expected values over the MDC channel");
    Ok(())
}

// ---------------------------------------------------------------------------
// aeron-rs (pure-Rust) backend
// ---------------------------------------------------------------------------

/// The `aeron-rs` backend round-trips through the graph. Its subscriber reports
/// `supports_graph_thread_poll() == false`, so the requested `Spin` mode is
/// silently downgraded to `Threaded` — that downgrade is what this exercises
/// end-to-end.
#[test]
fn aeron_rs_roundtrip() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;
    let handle = AeronRsHandle::connect()?;
    let stream_id = 3001i32;

    let values = round_trip(
        handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?,
        AeronSubOptions::default(),
        1,
        2,
    )?;
    assert!(
        !values.is_empty(),
        "expected values via the aeron-rs backend, got: {values:?}"
    );
    Ok(())
}

/// `fragment_limit` round-trips through both backends' builders.
#[test]
fn fragment_limit_field_round_trips() -> anyhow::Result<()> {
    let _serial = serial_guard();
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let sub = handle.subscription(AERON_CHANNEL, 2101, CONNECT_TIMEOUT)?;
    assert_eq!(sub.fragment_limit(), 256, "rusteron default");
    let sub = handle
        .subscription(AERON_CHANNEL, 2102, CONNECT_TIMEOUT)?
        .with_fragment_limit(32);
    assert_eq!(sub.fragment_limit(), 32);

    let rs_handle = AeronRsHandle::connect()?;
    let rs_sub = rs_handle.subscription(AERON_CHANNEL, 3101, CONNECT_TIMEOUT)?;
    assert_eq!(rs_sub.fragment_limit(), 256, "aeron-rs default");
    let rs_sub = rs_handle
        .subscription(AERON_CHANNEL, 3102, CONNECT_TIMEOUT)?
        .with_fragment_limit(64);
    assert_eq!(rs_sub.fragment_limit(), 64);
    Ok(())
}
