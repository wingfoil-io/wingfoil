//! Cross-*language* parity tests: a next-Rust graph on one side of a ZMQ socket
//! and the `wingfoil` Python bindings on the other, in a separate process.
//!
//! Port of legacy's `zmq/integration_tests.rs::cross_lang_tests` — the same four
//! cases (direct pub/sub each way, then the same pair through etcd discovery),
//! with legacy's `wingfoil` module swapped for `wingfoil` and its
//! wiring translated to next's `Graph`-first Python API.
//!
//! Together with `zmq_cross_engine_integration.rs` these close deviation-
//! register **C2** (cutover-plan row 2.3). The split is worth keeping straight:
//!
//! - *cross-engine* proves next and **legacy** agree on the wire, and dies with
//!   the legacy tree.
//! - *cross-language* (this file) proves next-Rust and **next-Python** agree,
//!   and survives the cutover as the permanent interop test.
//!
//! Requires the Python module to be importable — run
//! `maturin develop` in `crates/wingfoil-python` first. A missing module is
//! a **failure**, never a skip: a silently-skipped interop test is how a broken
//! binding reaches a release green.
//!
//! ```sh
//! cd crates/wingfoil-python && maturin develop && cd -
//! cargo test --manifest-path crates/wingfoil/Cargo.toml --features zmq-cross-lang-test \
//!   -- --test-threads=1 --nocapture
//! ```
//!
//! Ports 5741–5744 (see the port table in `src/adapters/zmq/CLAUDE.md`).
#![cfg(feature = "zmq-cross-lang-test")]

use std::process::{Command, Output, Stdio};
use std::time::Duration;

use wingfoil::adapters::zmq::{ZeroMqPub, zmq_sub};
use wingfoil::prelude::*;
use wingfoil::{RunFor, RunMode};

const PUB_RUN: Duration = Duration::from_secs(3);
const SUB_RUN: Duration = Duration::from_millis(1500);
const TICK: Duration = Duration::from_millis(50);
/// Head start for a publisher to bind before its peer connects — ZMQ's
/// slow-joiner problem. Matches `zmq_integration.rs`'s `SUB_SETTLE`.
const BIND_SETTLE: Duration = Duration::from_millis(1500);

/// Fail loudly if the bindings are not installed, naming the fix. Legacy's
/// `require_python` does the same; a skip here would hide a broken binding.
fn require_python() {
    let ok = Command::new("python3")
        .args(["-c", "import wingfoil"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(
        ok,
        "zmq-cross-lang-test is enabled but `import wingfoil` failed. \
         Run `maturin develop` in crates/wingfoil-python first."
    );
}

fn run_python(script: &str) -> Output {
    Command::new("python3")
        .args(["-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("failed to execute python3")
}

fn spawn_python(script: &str) -> std::process::Child {
    Command::new("python3")
        .args(["-c", script])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn python3")
}

/// Surface the child's stdout *and* stderr on failure — a bare exit code from a
/// subprocess assertion is close to undebuggable in CI.
fn assert_python_ok(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed (exit {}):\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The Python-side assertion, shared by both directions where Python subscribes.
/// Consecutiveness, not an absolute start: the subscriber joins mid-stream.
fn python_consecutive_assertions() -> &'static str {
    r#"
assert len(items) >= 3, f"expected >= 3 items, got {len(items)}"
nums = [int(b) for b in items]
for a, b in zip(nums, nums[1:]):
    assert b == a + 1, f"non-consecutive: {a}, {b}"
"#
}

fn spawn_rust_publisher(port: u16) -> std::thread::JoinHandle<anyhow::Result<()>> {
    std::thread::spawn(move || {
        let g = GraphBuilder::new();
        let _sink = g
            .ticker(TICK)
            .count()
            .map(|n: &u64| format!("{n}").into_bytes())
            .zmq_pub(port, ());
        g.build().run(RunMode::RealTime, RunFor::Duration(PUB_RUN))
    })
}

fn collect_rust_counters(address: &str) -> Vec<u64> {
    let g = GraphBuilder::new();
    let (data, _status) =
        zmq_sub::<Vec<u8>>(&g, RunMode::RealTime, address).expect("zmq_sub failed");
    let received = data.collapse_accumulate();
    let mut runner = g.build();
    runner
        .run(RunMode::RealTime, RunFor::Duration(SUB_RUN))
        .expect("rust subscriber run failed");
    runner
        .value(&received)
        .into_iter()
        .map(|b| {
            String::from_utf8(b)
                .expect("payload is not valid utf8")
                .parse()
                .expect("payload is not a counter")
        })
        .collect()
}

fn assert_consecutive(values: &[u64], label: &str) {
    assert!(
        values.len() >= 3,
        "{label}: expected >= 3 messages, got {} ({values:?})",
        values.len()
    );
    for w in values.windows(2) {
        assert_eq!(
            w[1],
            w[0] + 1,
            "{label}: non-consecutive counters {} then {} (full run: {values:?})",
            w[0],
            w[1]
        );
    }
}

#[test]
fn rust_pub_python_sub_direct() {
    require_python();
    _ = env_logger::try_init();
    let port = 5741u16;

    let publisher = spawn_rust_publisher(port);
    std::thread::sleep(BIND_SETTLE);

    let script = format!(
        r#"
import wingfoil as wf
g = wf.Graph()
items = []
data, _status = wf.zmq_sub(g, "tcp://127.0.0.1:{port}")
data.inspect(items.extend)
g.run(realtime=True, duration_nanos={sub_nanos})
{assertions}
"#,
        sub_nanos = SUB_RUN.as_nanos(),
        assertions = python_consecutive_assertions(),
    );
    let output = run_python(&script);
    assert_python_ok(&output, "rust_pub_python_sub_direct");

    publisher
        .join()
        .expect("rust publisher thread panicked")
        .expect("rust publisher run failed");
}

#[test]
fn python_pub_rust_sub_direct() {
    require_python();
    _ = env_logger::try_init();
    let port = 5742u16;

    let script = format!(
        r#"
import wingfoil as wf
g = wf.Graph()
counter = g.counter(period_nanos={tick_nanos})
wf.zmq_pub(counter.map(lambda n: str(n).encode()), {port}, bind_address="127.0.0.1")
g.run(realtime=True, duration_nanos={pub_nanos})
"#,
        tick_nanos = TICK.as_nanos(),
        pub_nanos = PUB_RUN.as_nanos(),
    );
    let child = spawn_python(&script);
    std::thread::sleep(BIND_SETTLE);

    let values = collect_rust_counters(&format!("tcp://127.0.0.1:{port}"));
    assert_consecutive(&values, "python_pub_rust_sub_direct");

    let output = child.wait_with_output().expect("failed to wait on python3");
    assert_python_ok(&output, "python_pub_rust_sub_direct");
}

/// The etcd-discovery half. Same two directions, with the publisher's address
/// resolved through etcd instead of hard-coded — legacy's
/// `cross_lang_tests::etcd`.
#[cfg(feature = "zmq-cross-lang-etcd-test")]
mod etcd {
    use super::*;

    use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};
    use wingfoil::adapters::etcd::EtcdConnection;
    use wingfoil::adapters::zmq::EtcdRegistry;

    /// Same image and readiness probe as `zmq_etcd_integration.rs`.
    fn start_etcd() -> anyhow::Result<(impl Drop, String)> {
        let container = GenericImage::new("quay.io/coreos/etcd", "v3.5.17")
            .with_wait_for(WaitFor::message_on_stderr("ready to serve client requests"))
            .with_env_var("ETCD_ADVERTISE_CLIENT_URLS", "http://0.0.0.0:2379")
            .with_env_var("ETCD_LISTEN_CLIENT_URLS", "http://0.0.0.0:2379")
            .with_cmd(["/usr/local/bin/etcd"])
            .start()?;
        let port = container.get_host_port_ipv4(2379)?;
        Ok((container, format!("http://127.0.0.1:{port}")))
    }

    /// Block until `key` is registered, so a subscriber never looks up an
    /// address its publisher has not written yet.
    fn wait_for_key(endpoint: &str, key: &str, timeout: Duration) -> anyhow::Result<String> {
        let conn = EtcdConnection::new(endpoint.to_string());
        let deadline = std::time::Instant::now() + timeout;
        loop {
            if let Ok(Some(value)) = read_key(&conn, key) {
                return Ok(value);
            }
            if std::time::Instant::now() >= deadline {
                anyhow::bail!("timed out waiting for etcd key {key:?}");
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn read_key(conn: &EtcdConnection, key: &str) -> anyhow::Result<Option<String>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let mut client = etcd_client::Client::connect(&conn.endpoints, None).await?;
            let resp = client.get(key, None).await?;
            Ok(match resp.kvs().first() {
                Some(kv) => Some(kv.value_str()?.to_string()),
                None => None,
            })
        })
    }

    #[test]
    fn rust_pub_python_sub_etcd() {
        require_python();
        _ = env_logger::try_init();
        let (_container, endpoint) = start_etcd().expect("etcd container failed to start");
        let port = 5743u16;
        let service = "cross-lang/rust-pub";

        let ep = endpoint.clone();
        let publisher = std::thread::spawn(move || {
            let g = GraphBuilder::new();
            let _sink = g
                .ticker(TICK)
                .count()
                .map(|n: &u64| format!("{n}").into_bytes())
                .zmq_pub(port, (service, EtcdRegistry::new(EtcdConnection::new(ep))));
            g.build().run(RunMode::RealTime, RunFor::Duration(PUB_RUN))
        });

        wait_for_key(&endpoint, service, Duration::from_secs(5))
            .expect("publisher never registered in etcd");

        let script = format!(
            r#"
import wingfoil as wf
g = wf.Graph()
items = []
data, _status = wf.zmq_sub_etcd(g, "{service}", "{endpoint}")
data.inspect(items.extend)
g.run(realtime=True, duration_nanos={sub_nanos})
{assertions}
"#,
            sub_nanos = SUB_RUN.as_nanos(),
            assertions = python_consecutive_assertions(),
        );
        let output = run_python(&script);
        assert_python_ok(&output, "rust_pub_python_sub_etcd");

        publisher
            .join()
            .expect("rust publisher thread panicked")
            .expect("rust publisher run failed");
    }

    #[test]
    fn python_pub_rust_sub_etcd() {
        require_python();
        _ = env_logger::try_init();
        let (_container, endpoint) = start_etcd().expect("etcd container failed to start");
        let port = 5744u16;
        let service = "cross-lang/python-pub";

        let script = format!(
            r#"
import wingfoil as wf
g = wf.Graph()
counter = g.counter(period_nanos={tick_nanos})
# Keyword arguments deliberately: next's `zmq_pub_etcd` takes
# (port, name, endpoint), where legacy's took (name, port, endpoint). Naming
# them means this reads correctly against either and cannot silently swap
# a port for a service name.
wf.zmq_pub_etcd(
    counter.map(lambda n: str(n).encode()),
    port={port},
    name="{service}",
    endpoint="{endpoint}",
    bind_address="127.0.0.1",
)
g.run(realtime=True, duration_nanos={pub_nanos})
"#,
            tick_nanos = TICK.as_nanos(),
            pub_nanos = PUB_RUN.as_nanos(),
        );
        let child = spawn_python(&script);

        wait_for_key(&endpoint, service, Duration::from_secs(10))
            .expect("python publisher never registered in etcd");

        let g = GraphBuilder::new();
        let (data, _status) = zmq_sub::<Vec<u8>>(
            &g,
            RunMode::RealTime,
            (service, EtcdRegistry::new(EtcdConnection::new(endpoint))),
        )
        .expect("zmq_sub via etcd failed");
        let received = data.collapse_accumulate();
        let mut runner = g.build();
        runner
            .run(RunMode::RealTime, RunFor::Duration(SUB_RUN))
            .expect("rust subscriber run failed");
        let values: Vec<u64> = runner
            .value(&received)
            .into_iter()
            .map(|b| {
                String::from_utf8(b)
                    .expect("payload is not valid utf8")
                    .parse()
                    .expect("payload is not a counter")
            })
            .collect();
        assert_consecutive(&values, "python_pub_rust_sub_etcd");

        let output = child.wait_with_output().expect("failed to wait on python3");
        assert_python_ok(&output, "python_pub_rust_sub_etcd");
    }
}
