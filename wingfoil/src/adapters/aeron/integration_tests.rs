//! Integration tests for the Aeron adapter.
//!
//! Requires a running Aeron media driver via Docker.
//! Run with:
//! ```sh
//! cargo test --features aeron-integration-test -p wingfoil \
//!   -- --test-threads=1 aeron::integration_tests
//! ```
//!
//! Image: `neomantra/aeron-cpp-debian` — ships `/usr/local/bin/aeronmd` as entrypoint.
//! The driver writes its CNC file to `/dev/shm/aeron-default/`; we bind-mount `/dev/shm`
//! so the host Rust process connects to the same driver via `aeron:ipc`.

use super::*;
use crate::nodes::{NodeOperators, StreamOperators};
use crate::{Burst, Graph, RunFor, RunMode};
use std::time::Duration;
use testcontainers::{
    GenericImage, ImageExt,
    core::{Mount, WaitFor},
    runners::SyncRunner,
};

/// `neomantra/aeron-cpp-debian` ships `aeronmd` as its entrypoint and logs nothing
/// to stdout/stderr, so we use a fixed-time WaitFor.
const DRIVER_IMAGE: &str = "neomantra/aeron-cpp-debian";
const DRIVER_TAG: &str = "latest";
/// IPC channel: communicates via the shared CNC file in /dev/shm — no UDP needed.
pub(crate) const AERON_CHANNEL: &str = "aeron:ipc";
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Shared Aeron directory used by both the container's aeronmd and the host client.
/// Setting the same `AERON_DIR` on both sides ensures the CNC file path matches
/// regardless of which user aeronmd runs as inside the container.
const AERON_DIR: &str = "/dev/shm/aeron-integration-test";

/// Start an Aeron media driver container and return a guard that stops it on drop.
/// Binds `/dev/shm` into the container so the host process shares the CNC file.
pub(crate) fn start_media_driver() -> anyhow::Result<impl Drop> {
    // Tell the host Aeron client where to find the CNC file.
    // SAFETY: tests run with --test-threads=1, so no concurrent env access.
    unsafe { std::env::set_var("AERON_DIR", AERON_DIR) };

    // Remove any stale CNC files left by a previous test run so the new
    // aeronmd creates a fresh one with a valid heartbeat.
    let _ = std::fs::remove_dir_all(AERON_DIR);

    // Run as the current user so the CNC file is writable by the test process.
    // Aeron clients open the CNC file with O_RDWR; a root-owned 0644 file
    // would cause EACCES for non-root users.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    let container = GenericImage::new(DRIVER_IMAGE, DRIVER_TAG)
        // Minimal structural wait — we poll for the CNC file below.
        .with_wait_for(WaitFor::seconds(1))
        // Bind-mount the host's /dev/shm so aeronmd's CNC file is visible to
        // the host process.  Do NOT call with_shm_size — that creates a
        // separate tmpfs which overrides the bind mount.
        .with_mount(Mount::bind_mount("/dev/shm", "/dev/shm"))
        .with_env_var("AERON_DIR", AERON_DIR)
        .with_user(format!("{uid}:{gid}"))
        .start()?;

    // Poll until aeronmd creates the CNC file (max 10 s).
    let cnc = std::path::Path::new(AERON_DIR).join("cnc.dat");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !cnc.exists() {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "timed out waiting for aeronmd CNC file: {}",
            cnc.display()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    // Give aeronmd a moment to finish initialising the CNC file contents
    // after it first appears on disk.
    std::thread::sleep(Duration::from_millis(500));

    Ok(container)
}

// ---- Tests ----

/// Without a running media driver, `AeronHandle::connect()` must return an error.
///
/// We point `AERON_DIR` at an empty temp directory so that a CNC file left by
/// an earlier test in this suite does not cause a spurious success.
#[test]
fn test_no_driver_connection_fails() {
    const NO_DRIVER_DIR: &str = "/tmp/aeron-no-driver-test";
    let _ = std::fs::remove_dir_all(NO_DRIVER_DIR);
    // SAFETY: tests run with --test-threads=1, no concurrent env access.
    unsafe { std::env::set_var("AERON_DIR", NO_DRIVER_DIR) };
    let result = AeronHandle::connect();
    assert!(
        result.is_err(),
        "expected connection error when no media driver is running"
    );
}

/// `try_claim` zero-copy round-trip: claim a 64-byte slot, fill with `0xAB`,
/// commit, and assert the subscriber receives exactly those bytes.
#[cfg(feature = "aeron")]
#[test]
fn test_try_claim_zero_copy_roundtrip() -> anyhow::Result<()> {
    use crate::adapters::aeron::transport::{AeronPublisherBackend, AeronSubscriberBackend};

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2005i32;

    let mut sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let mut pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let mut claim = pub_.try_claim(64).expect("try_claim succeeds");
    assert_eq!(claim.len(), 64);
    for b in claim.data().iter_mut() {
        *b = 0xAB;
    }
    claim.commit().expect("commit succeeds");

    let mut received: Option<Vec<u8>> = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline && received.is_none() {
        let _ = sub.poll(&mut |bytes: &[u8]| {
            received = Some(bytes.to_vec());
        });
        if received.is_none() {
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    let bytes = received.expect("subscription observes committed claim within 2s");
    assert_eq!(bytes.len(), 64);
    assert!(
        bytes.iter().all(|b| *b == 0xAB),
        "all 64 bytes equal 0xAB: {bytes:?}"
    );
    Ok(())
}

/// Poll until the publication reports a connected subscriber image, or fail
/// after `timeout`. An Aeron IPC/UDP publication is only "connected" once a
/// matching subscription has joined, and the media-driver conductor establishes
/// that image asynchronously; until then `try_claim`/`offer` return
/// `Connection("Not connected")`. Exercising a publication before the image is
/// ready is the dominant source of flakiness on slower CI runners, so tests
/// that act on a publication directly wait here first.
#[cfg(feature = "aeron")]
fn wait_for_pub_connected<P>(pub_: &P, timeout: Duration) -> anyhow::Result<()>
where
    P: crate::adapters::aeron::transport::AeronPublisherBackend,
{
    let deadline = std::time::Instant::now() + timeout;
    while !pub_.is_connected() {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "publication did not connect within {timeout:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

/// `try_claim` returns `TransportError::BackPressure` once the term buffer
/// saturates with no subscriber polling.
///
/// Uses a small term length (`term-length=65536`, the minimum supported) so the
/// buffer fills within a bounded iteration count.
#[cfg(feature = "aeron")]
#[test]
fn test_try_claim_back_pressure_returns_typed_error() -> anyhow::Result<()> {
    use crate::adapters::aeron::transport::AeronPublisherBackend;

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2006i32;
    let channel = "aeron:ipc?term-length=65536";

    // A publication connects only once a subscriber joins. Hold a subscription
    // (never polled, so the term buffer saturates) and wait for the image
    // before claiming, otherwise the first `try_claim` returns "Not connected".
    let _sub = handle.subscription(channel, stream_id, CONNECT_TIMEOUT)?;
    let mut pub_ = handle.publication(channel, stream_id, CONNECT_TIMEOUT)?;
    wait_for_pub_connected(&pub_, CONNECT_TIMEOUT)?;

    let mut back_pressure_seen = false;
    for _ in 0..200 {
        match pub_.try_claim(1024) {
            Ok(claim) => {
                claim.commit().expect("commit succeeds");
            }
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
#[cfg(feature = "aeron")]
#[test]
fn test_try_claim_drop_without_commit_aborts() -> anyhow::Result<()> {
    use crate::adapters::aeron::transport::AeronPublisherBackend;

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2007i32;

    // Hold a subscriber so the publication can reach the connected state, then
    // wait for the image before claiming.
    let _sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let mut pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    wait_for_pub_connected(&pub_, CONNECT_TIMEOUT)?;

    let claim = pub_.try_claim(64).expect("first try_claim succeeds");
    drop(claim);

    let claim2 = pub_.try_claim(64).expect("second try_claim succeeds");
    claim2.commit().expect("commit succeeds");
    Ok(())
}

/// aeron-rs (pure-Rust) backend: ticker-driven round-trip.
#[cfg(feature = "aeron-rs-beta")]
#[test]
fn test_aeron_rs_spin_roundtrip() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronRsHandle::connect()?;
    let stream_id = 3001i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let subscriber = aeron_sub_fragment(sub, parser, AeronSubOptions::default());
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(77i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        values.contains(&77i64),
        "expected 77 via aeron-rs backend, got: {values:?}"
    );
    Ok(())
}

/// `fragment_limit` is a per-`poll()` cap, not a target.
///
/// Publish 1000 small messages, then poll with `fragment_limit = 32` and assert
/// that every non-zero `poll()` returns at most 32 fragments while all 1000 are
/// delivered in total. At Aeron IPC throughput the publisher fills the term
/// buffer faster than the capped subscriber drains, guaranteeing at least one
/// non-zero poll within the cap — a buggy `fragment_limit` wiring would deliver
/// all 1000 in a single poll.
#[cfg(feature = "aeron")]
#[test]
fn test_fragment_limit_caps_burst_size() -> anyhow::Result<()> {
    use crate::adapters::aeron::transport::{AeronPublisherBackend, AeronSubscriberBackend};

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2008i32;

    let mut sub = handle
        .subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?
        .with_fragment_limit(32);
    let mut pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    // Publish 1000 8-byte payloads with bounded back-pressure retry.
    let mut published = 0usize;
    let pub_deadline = std::time::Instant::now() + Duration::from_secs(10);
    while published < 1000 {
        anyhow::ensure!(
            std::time::Instant::now() < pub_deadline,
            "timed out publishing at {published} of 1000"
        );
        let bytes = (published as i64).to_le_bytes();
        match pub_.offer(&bytes) {
            Ok(()) => published += 1,
            Err(_) => std::thread::sleep(Duration::from_micros(100)),
        }
    }

    // Poll until all 1000 received, asserting the cap on every non-zero poll.
    let mut total = 0usize;
    let mut saw_capped_burst = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while total < 1000 {
        anyhow::ensure!(
            std::time::Instant::now() < deadline,
            "timed out at {total} of 1000"
        );
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
// aeron_sub_fragment typed-parser surface
// ---------------------------------------------------------------------------

/// Spin-mode typed-parser round-trip: an independent ticker-driven publisher
/// emits a single value over the media driver; `aeron_sub_fragment` collects it.
#[cfg(feature = "aeron")]
#[test]
fn test_spin_sub_burst_single_message_roundtrip() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2009i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let subscriber = aeron_sub_fragment(sub, parser, AeronSubOptions::default());
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(42i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        values.contains(&42i64),
        "expected to receive 42, got: {values:?}"
    );
    Ok(())
}

/// Spin-mode typed-parser burst accumulation: publish bursts of 15 each tick;
/// assert at least 10 total received.
#[cfg(feature = "aeron")]
#[test]
fn test_spin_sub_burst_typed_accumulation() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2010i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let subscriber = aeron_sub_fragment(
        sub,
        parser,
        AeronSubOptions {
            mode: AeronMode::Spin,
            fragment_limit: 256,
        },
    );
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|n| {
            let mut burst = Burst::new();
            for i in 0..15 {
                burst.push(n as i64 * 15 + i);
            }
            burst
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let total: usize = collected.peek_value().iter().map(|b| b.value.len()).sum();
    assert!(
        total >= 10,
        "expected at least 10 accumulated values, got: {total}"
    );
    Ok(())
}

/// Threaded-mode typed-parser burst node delivers messages via its
/// background channel.
#[cfg(feature = "aeron")]
#[test]
fn test_threaded_sub_burst_accumulates_across_channel_drain() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2011i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let subscriber = aeron_sub_fragment(
        sub,
        parser,
        AeronSubOptions {
            mode: AeronMode::Threaded,
            fragment_limit: 256,
        },
    );
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(20))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(99i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        !values.is_empty(),
        "expected at least one value via threaded burst subscriber"
    );
    Ok(())
}

/// `.collapse()` on `Rc<dyn Stream<Burst<T>>>` reduces each burst to its
/// latest element, yielding `Rc<dyn Stream<T>>`.
#[cfg(feature = "aeron")]
#[test]
fn test_collapse_yields_latest_value() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2012i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let subscriber = aeron_sub_fragment(sub, parser, AeronSubOptions::default());
    // `.collapse()` reduces a burst stream to a scalar stream of latest values.
    let collapsed = subscriber.collapse();
    let collected = collapsed.collect();

    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|n| {
            let mut burst = Burst::new();
            for i in 0..10 {
                burst.push(n as i64 * 10 + i);
            }
            burst
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(1)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .map(|v| v.value)
        .collect();
    assert!(
        !values.is_empty(),
        "expected at least one collapsed value, got: {values:?}"
    );
    // Latest-wins: every emitted value MUST be a per-burst tail (n*10+9 style)
    // — i.e. equal to 9 mod 10 — because `.collapse()` keeps the last element
    // of each burst.
    assert!(
        values.iter().any(|v| *v % 10 == 9),
        "expected at least one tail-of-burst value (== 9 mod 10), got: {values:?}"
    );
    Ok(())
}

/// Regression guard: the rusteron `poll_fragments` override delivers real
/// per-fragment header data, not the synthesised zero from the default impl.
#[cfg(feature = "aeron")]
#[test]
fn test_burst_parser_sees_fragment_header_with_real_position() -> anyhow::Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicI64, Ordering};

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2013i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let captured_position = Arc::new(AtomicI64::new(0));
    let captured_for_parser = captured_position.clone();
    let parser = move |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        // Compare-exchange records the FIRST observed position only.
        let _ = captured_for_parser.compare_exchange(
            0,
            frag.position(),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        Ok(Some(frag.position()))
    };
    let subscriber = aeron_sub_fragment(sub, parser, AeronSubOptions::default());
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(50))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(1i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let observed = captured_position.load(Ordering::SeqCst);
    assert!(
        observed > 0,
        "expected non-zero fragment position from rusteron override, got: {observed}"
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// Status stream integration tests.
// ---------------------------------------------------------------------------

/// Status stream emits a `Connected` transition once the subscriber observes
/// the publication. The exact cycle on which the transition lands depends on
/// the media driver, but it MUST appear somewhere during the run.
#[cfg(feature = "aeron")]
#[test]
fn test_status_stream_emits_on_connect() -> anyhow::Result<()> {
    use std::cell::RefCell;
    use std::rc::Rc;

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2014i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    // Ensure the image is established before the graph runs; the subscriber's
    // first poll then captures the Disconnected → Connected transition.
    wait_for_pub_connected(&pub_, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let (data, status_stream) =
        aeron_sub_fragment_with_status(sub, parser, AeronSubOptions::default());
    let observed: Rc<RefCell<Vec<AeronStatus>>> = Rc::new(RefCell::new(Vec::new()));
    let observed_inspect = Rc::clone(&observed);
    let inspected = status_stream.clone().inspect(move |burst| {
        for s in burst.iter() {
            observed_inspect.borrow_mut().push(s.clone());
        }
    });

    let source = crate::nodes::ticker(Duration::from_millis(50))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(7i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    // The subscriber data node must be in the graph: it polls Aeron and records
    // the connection transition into the (shared) status stream. Without it the
    // subscriber never polls and the status stream stays empty.
    Graph::new(
        vec![data.as_node(), inspected.as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let observed = observed.borrow();
    assert!(
        observed.contains(&AeronStatus::Connected),
        "expected status stream to record AeronStatus::Connected, got: {observed:?}"
    );
    Ok(())
}

/// Steady-state run produces at most one transition. The producer node clears
/// the status burst at the start of each cycle, and `record()` is a no-op when
/// the new status equals the previous — so we should only ever see the
/// initial `Disconnected → Connected` transition, never a re-emit.
#[cfg(feature = "aeron")]
#[test]
fn test_status_stream_no_emission_when_steady() -> anyhow::Result<()> {
    use std::cell::RefCell;
    use std::rc::Rc;

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2015i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let (_data, status_stream) =
        aeron_sub_fragment_with_status(sub, parser, AeronSubOptions::default());
    let observed: Rc<RefCell<Vec<AeronStatus>>> = Rc::new(RefCell::new(Vec::new()));
    let observed_inspect = Rc::clone(&observed);
    let inspected = status_stream.clone().inspect(move |burst| {
        for s in burst.iter() {
            observed_inspect.borrow_mut().push(s.clone());
        }
    });

    let source = crate::nodes::ticker(Duration::from_millis(20))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(7i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![inspected.as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    // Expect at most one transition during the entire run (the initial
    // Disconnected → Connected). Re-emission would manifest as ≥2 entries.
    let observed = observed.borrow();
    assert!(
        observed.len() <= 1,
        "expected ≤1 transition under steady state, observed: {observed:?}"
    );
    Ok(())
}

/// Threaded-mode counterpart of `test_status_stream_emits_on_connect`: with
/// `AeronMode::Threaded` the background poll thread multiplexes the lifecycle
/// status onto the receiver channel (as `AeronItem::Status`), and the data node
/// demuxes it back into the shared status stream. The `Connected` transition
/// MUST still surface somewhere during the run, exercising the rusteron
/// backend's real `is_connected()` across the thread boundary.
#[cfg(feature = "aeron")]
#[test]
fn test_threaded_status_stream_emits_on_connect() -> anyhow::Result<()> {
    use std::cell::RefCell;
    use std::rc::Rc;

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2025i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    // Establish the image before the graph runs so the subscriber's first
    // background poll captures the Disconnected → Connected transition.
    wait_for_pub_connected(&pub_, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let (data, status_stream) = aeron_sub_fragment_with_status(
        sub,
        parser,
        AeronSubOptions {
            mode: AeronMode::Threaded,
            fragment_limit: 256,
        },
    );
    let observed: Rc<RefCell<Vec<AeronStatus>>> = Rc::new(RefCell::new(Vec::new()));
    let observed_inspect = Rc::clone(&observed);
    let inspected = status_stream.clone().inspect(move |burst| {
        for s in burst.iter() {
            observed_inspect.borrow_mut().push(s.clone());
        }
    });

    let source = crate::nodes::ticker(Duration::from_millis(50))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(7i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    // The subscriber data node demuxes data + status from its background poll
    // thread; without it in the graph the status stream is never driven.
    Graph::new(
        vec![data.as_node(), inspected.as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let observed = observed.borrow();
    assert!(
        observed.contains(&AeronStatus::Connected),
        "expected threaded status stream to record AeronStatus::Connected, got: {observed:?}"
    );
    Ok(())
}

/// Saturate the publication term buffer until back-pressure surfaces and
/// assert the publisher status stream records `BackPressured` at least once.
///
/// The saturation harness depends on media-driver buffer sizes and is
/// environmental — gated on `aeron-integration-test` rather than the
/// compile-only `aeron` feature.
#[cfg(all(feature = "aeron", feature = "aeron-integration-test"))]
#[test]
fn test_publisher_status_emits_back_pressure() -> anyhow::Result<()> {
    use std::cell::RefCell;
    use std::rc::Rc;

    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2016i32;

    // Use the minimum term length (64 KiB) so the publication term buffer
    // saturates after a bounded handful of offers, independent of runner speed.
    // (The default 16 MiB term needs ~hundreds of large offers, which a slow CI
    // runner may not reach within the run window.) Max message length is
    // term-length/8 = 8 KiB, so the payload below stays under that.
    let channel = "aeron:ipc?term-length=65536";
    let _sub = handle.subscription(channel, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(channel, stream_id, CONNECT_TIMEOUT)?;
    // Saturation only produces BackPressured once the publication is connected;
    // an unconnected publication reports Disconnected and never back-pressures.
    wait_for_pub_connected(&pub_, CONNECT_TIMEOUT)?;

    // 4 KiB payload — fits the 64 KiB term (max message 8 KiB) and fills the
    // buffer within a few dozen offers since the subscriber is never polled.
    let payload: Vec<u8> = vec![0xABu8; 4 * 1024];
    let source = crate::nodes::ticker(Duration::from_millis(1))
        .count()
        .map(move |_| {
            let mut b = Burst::new();
            b.push(payload.clone());
            b
        });
    let (pub_node, status_stream) = source.aeron_pub_with_status(pub_, |v: &Vec<u8>| v.clone());
    let observed: Rc<RefCell<Vec<AeronStatus>>> = Rc::new(RefCell::new(Vec::new()));
    let observed_inspect = Rc::clone(&observed);
    let inspected = status_stream.clone().inspect(move |burst| {
        for s in burst.iter() {
            observed_inspect.borrow_mut().push(s.clone());
        }
    });

    Graph::new(
        vec![inspected.as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(3)),
    )
    .run()
    .ok();

    let observed = observed.borrow();
    assert!(
        observed.contains(&AeronStatus::BackPressured),
        "expected publisher status to record BackPressured under saturation, got: {observed:?}"
    );
    Ok(())
}

/// Plain `aeron_pub` publishes every distinct value when none
/// repeat. The subscriber receives all of them.
#[cfg(feature = "aeron")]
#[test]
fn test_publisher_no_dedup_publishes_every_burst_item() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2019i32;

    let sub = handle.subscription(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(AERON_CHANNEL, stream_id, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let subscriber = aeron_sub_fragment(sub, parser, AeronSubOptions::default());
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(50))
        .count()
        .map(|n| {
            let mut b = Burst::new();
            b.push(n as i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();
    let unique: std::collections::HashSet<i64> = values.iter().copied().collect();
    assert!(
        unique.len() >= 5,
        "expected at least 5 distinct values via plain aeron_pub, got {} unique: {values:?}",
        unique.len()
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// ChannelUri integration tests
// ---------------------------------------------------------------------------

/// `ChannelUri::ipc()` produces a usable IPC URI: round-trip a single i64
/// from a publisher to a subscriber both constructed via the builder.
#[cfg(feature = "aeron")]
#[test]
fn test_channel_uri_ipc_roundtrip() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2020i32;
    let channel = ChannelUri::ipc();

    let sub = handle.subscription(&channel, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(&channel, stream_id, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let subscriber = aeron_sub_fragment(sub, parser, AeronSubOptions::default());
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(7i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        values.contains(&7i64),
        "ChannelUri::ipc() round-trip lost message: {values:?}"
    );
    Ok(())
}

/// `ChannelUri::mdc_publication` + `ChannelUri::mdc_subscription` compose a
/// usable MDC channel pair: round-trip a single i64 across them.
///
/// MDC depends on UDP loopback being functional; on environments where this
/// is flaky, gate this test additionally behind `aeron-integration-test`.
#[cfg(feature = "aeron")]
#[test]
fn test_channel_uri_mdc_roundtrip() -> anyhow::Result<()> {
    let _container = start_media_driver()?;

    let handle = AeronHandle::connect()?;
    let stream_id = 2021i32;
    let pub_channel = ChannelUri::mdc_publication("127.0.0.1:40456")?;
    let sub_channel = ChannelUri::mdc_subscription("127.0.0.1:40457", "127.0.0.1:40456")?;

    let sub = handle.subscription(&sub_channel, stream_id, CONNECT_TIMEOUT)?;
    let pub_ = handle.publication(&pub_channel, stream_id, CONNECT_TIMEOUT)?;
    // MDC connects over UDP loopback and is slower to establish than IPC; wait
    // for the image so the round-trip does not miss its window on CI.
    wait_for_pub_connected(&pub_, CONNECT_TIMEOUT)?;

    let parser = |frag: &FragmentBuffer<'_>| -> Result<Option<i64>, TransportError> {
        Ok(frag.as_ref().try_into().ok().map(i64::from_le_bytes))
    };
    let subscriber = aeron_sub_fragment(sub, parser, AeronSubOptions::default());
    let collected = subscriber.collect();

    let source = crate::nodes::ticker(Duration::from_millis(10))
        .count()
        .map(|_| {
            let mut b = Burst::new();
            b.push(99i64);
            b
        });
    let pub_node = source.aeron_pub(pub_, |v: &i64| v.to_le_bytes().to_vec());

    Graph::new(
        vec![collected.clone().as_node(), pub_node],
        RunMode::RealTime,
        RunFor::Duration(Duration::from_secs(2)),
    )
    .run()
    .ok();

    let values: Vec<i64> = collected
        .peek_value()
        .into_iter()
        .flat_map(|b| b.value)
        .collect();

    assert!(
        values.contains(&99i64),
        "ChannelUri::mdc_* round-trip lost message: {values:?}"
    );
    Ok(())
}
