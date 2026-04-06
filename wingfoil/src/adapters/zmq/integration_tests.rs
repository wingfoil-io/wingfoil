use super::{ZeroMqPub, ZmqStatus, zmq_sub};
use crate::{Graph, Node, NodeOperators, RunFor, RunMode, StreamOperators, ticker};
use log::Level::Info;
use std::rc::Rc;
use std::time::Duration;

// --- ZMQ integration tests (ports 5556–5562) ---

#[test]
fn zmq_deserialization_error_propagates() {
    _ = env_logger::try_init();
    let port = 5562;

    std::thread::spawn(move || {
        let ctx = zmq::Context::new();
        let sock = ctx.socket(zmq::PUB).unwrap();
        sock.bind(&format!("tcp://127.0.0.1:{port}")).unwrap();
        std::thread::sleep(Duration::from_millis(200));
        for _ in 0..20 {
            sock.send("not valid bincode".as_bytes(), 0).unwrap();
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    let (data, _status) =
        zmq_sub::<u64>(format!("tcp://127.0.0.1:{port}")).expect("zmq_sub failed");
    let result = data
        .as_node()
        .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)));

    assert!(
        result.is_err(),
        "expected deserialization error to propagate"
    );
}

fn sender(period: Duration, port: u16) -> Rc<dyn Node> {
    ticker(period).count().logged("pub", Info).zmq_pub(port, ())
}

fn sender_with_delay(period: Duration, port: u16) -> Rc<dyn Node> {
    ticker(period)
        .count()
        .delay(Duration::from_millis(200))
        .logged("pub", Info)
        .zmq_pub(port, ())
}

fn receiver(address: &str) -> Rc<dyn Node> {
    let (data, _status) = zmq_sub::<u64>(address).expect("zmq_sub direct address should not fail");
    data.logged("sub", Info).collect().finally(|res, _| {
        let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
        println!("{values:?}");
        assert!(
            values.len() >= 5,
            "expected at least 5 items, got {}",
            values.len()
        );
        for window in values.windows(2) {
            assert_eq!(window[1], window[0] + 1, "expected consecutive integers");
        }
        Ok(())
    })
}

#[test]
fn zmq_same_thread() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(50);
    let port = 5556;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 10);
    Graph::new(
        vec![sender(period, port), receiver(&address)],
        RunMode::RealTime,
        run_for,
    )
    .print()
    .run()
    .unwrap();
}

#[test]
fn zmq_separate_threads() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(10);
    let port = 5557;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 10);
    let rf_send = run_for;
    let rf_rec = run_for;
    let rec = std::thread::spawn(move || receiver(&address).run(RunMode::RealTime, rf_rec));
    let send = std::thread::spawn(move || sender(period, port).run(RunMode::RealTime, rf_send));
    send.join().unwrap().unwrap();
    rec.join().unwrap().unwrap();
}

#[test]
fn zmq_pub_historical_mode_fails() {
    use crate::NanoTime;
    let result = sender(Duration::from_millis(10), 5558)
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
    let err = result.expect_err("expected historical mode to fail for zmq sender");
    let err_msg = format!("{err:?}");
    assert!(
        err_msg.contains("real-time"),
        "expected error to mention real-time, got: {err_msg}"
    );
}

#[test]
fn zmq_sub_historical_mode_fails() {
    use crate::NanoTime;
    let result = zmq_sub::<u64>("tcp://127.0.0.1:5559")
        .unwrap()
        .0
        .as_node()
        .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
    let err = result.expect_err("expected historical mode to fail for zmq receiver");
    let err_msg = format!("{err:?}");
    assert!(
        err_msg.contains("real-time"),
        "expected error to mention real-time, got: {err_msg}"
    );
}

#[test]
fn zmq_first_message_not_dropped() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(50);
    let port = 5560;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 15);
    let (data, _status) = zmq_sub::<u64>(&address).unwrap();
    let recv_node = data.collect().finally(|res, _| {
        let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
        assert!(!values.is_empty(), "no values received");
        assert_eq!(values[0], 1, "first message dropped: got {}", values[0]);
        Ok(())
    });
    Graph::new(
        vec![sender_with_delay(period, port), recv_node],
        RunMode::RealTime,
        run_for,
    )
    .run()
    .unwrap();
}

#[test]
fn zmq_first_message_not_dropped_no_delay() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(50);
    let port = 5564;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 15);
    let (data, _status) = zmq_sub::<u64>(&address).unwrap();
    let recv_node = data.collect().finally(|res, _| {
        let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
        assert!(!values.is_empty(), "no values received");
        assert_eq!(values[0], 1, "first message dropped: got {}", values[0]);
        Ok(())
    });
    // Uses sender() with NO delay — publisher sends immediately after bind.
    // This reliably drops message 1 due to the ZMQ slow-joiner problem.
    Graph::new(
        vec![sender(period, port), recv_node],
        RunMode::RealTime,
        run_for,
    )
    .run()
    .unwrap();
}

#[test]
fn zmq_reports_connected_status() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(50);
    let port = 5561;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 10);
    let (data, status) = zmq_sub::<u64>(&address).unwrap();
    let data_node = data.collect().finally(|res, _| {
        let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
        assert!(!values.is_empty(), "no data received");
        Ok(())
    });
    let status_node = status.collect().finally(|statuses, _| {
        let vs: Vec<ZmqStatus> = statuses.into_iter().map(|item| item.value).collect();
        assert!(
            vs.contains(&ZmqStatus::Connected),
            "expected Connected, got: {vs:?}"
        );
        Ok(())
    });
    Graph::new(
        vec![sender(period, port), data_node, status_node],
        RunMode::RealTime,
        run_for,
    )
    .run()
    .unwrap();
}

#[test]
fn zmq_sub_stops_cleanly_without_publisher_endofstream() {
    _ = env_logger::try_init();
    let port = 5563;
    let address = format!("tcp://127.0.0.1:{port}");

    // Publisher binds and holds the socket open for a bit, then drops it without
    // sending EndOfStream (simulates a publisher crash / SIGKILL).
    std::thread::spawn(move || {
        let ctx = zmq::Context::new();
        let sock = ctx.socket(zmq::PUB).unwrap();
        sock.bind(&format!("tcp://127.0.0.1:{port}")).unwrap();
        std::thread::sleep(Duration::from_millis(500));
        // sock drops here — no EndOfStream
    });

    let (data, _status) = zmq_sub::<u64>(&address).unwrap();

    let start = std::time::Instant::now();
    data.as_node()
        .run(
            RunMode::RealTime,
            RunFor::Duration(Duration::from_millis(300)),
        )
        .unwrap();
    let elapsed = start.elapsed();

    // Before the stop-flag fix this would hang indefinitely.
    // Allow generous headroom beyond the 200ms poll timeout.
    assert!(
        elapsed < Duration::from_secs(2),
        "subscriber took too long to stop: {elapsed:?}"
    );
}

// --- cross-language integration tests (ports 5580–5590) ---
//
// These tests validate that Rust and Python wingfoil processes can communicate
// over ZMQ. They spawn Python subprocesses and require that the `wingfoil`
// Python package is installed (via `maturin develop` in wingfoil-python/).
//
// Run:
//   cargo test --features zmq-cross-lang-test -p wingfoil \
//     -- --test-threads=1 zmq::integration_tests::cross_lang_tests
//
// With etcd:
//   cargo test --features zmq-cross-lang-etcd-test -p wingfoil \
//     -- --test-threads=1 zmq::integration_tests::cross_lang_tests

#[cfg(feature = "zmq-cross-lang-test")]
mod cross_lang_tests {
    use super::*;
    use std::process::{Command, Stdio};

    fn python_available() -> bool {
        Command::new("python3")
            .args(["-c", "import wingfoil"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn run_python(script: &str) -> std::process::Output {
        Command::new("python3")
            .args(["-c", script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .expect("failed to execute python3")
    }

    fn assert_python_ok(output: &std::process::Output, label: &str) {
        assert!(
            output.status.success(),
            "{label} failed (exit {}):\nstdout: {}\nstderr: {}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn zmq_rust_pub_python_sub_direct() {
        if !python_available() {
            eprintln!("SKIP: wingfoil python package not installed");
            return;
        }
        _ = env_logger::try_init();
        let port = 5580u16;

        // Rust publisher: sends counter bytes for 2s.
        let pub_handle = std::thread::spawn(move || {
            ticker(Duration::from_millis(50))
                .count()
                .map(|n: u64| format!("{n}").into_bytes())
                .zmq_pub(port, ())
                .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(2)))
        });

        // Let publisher bind.
        std::thread::sleep(Duration::from_millis(500));

        let script = format!(
            r#"
import wingfoil as wf
items = []
data, _status = wf.zmq_sub("tcp://127.0.0.1:{port}")
data.inspect(lambda msgs: items.extend(msgs)).run(realtime=True, duration=1.0)
assert len(items) >= 3, f"expected >= 3 items, got {{len(items)}}"
nums = [int(b) for b in items]
for a, b in zip(nums, nums[1:]):
    assert b == a + 1, f"non-consecutive: {{a}}, {{b}}"
"#
        );
        let output = run_python(&script);
        assert_python_ok(&output, "rust_pub_python_sub_direct");
        pub_handle.join().unwrap().unwrap();
    }

    #[test]
    fn zmq_python_pub_rust_sub_direct() {
        if !python_available() {
            eprintln!("SKIP: wingfoil python package not installed");
            return;
        }
        _ = env_logger::try_init();
        let port = 5581u16;

        // Python publisher: sends counter bytes for 2s.
        let script = format!(
            r#"
import wingfoil as wf
(
    wf.ticker(0.05)
    .count()
    .map(lambda n: str(n).encode())
    .zmq_pub({port})
    .run(realtime=True, duration=2.0)
)
"#
        );
        let child = Command::new("python3")
            .args(["-c", &script])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn python publisher");

        // Let Python publisher bind.
        std::thread::sleep(Duration::from_millis(500));

        // Rust subscriber.
        let address = format!("tcp://127.0.0.1:{port}");
        let (data, _status) = zmq_sub::<Vec<u8>>(&address).expect("zmq_sub failed");
        let recv_node = data.collect().finally(|res, _| {
            let values: Vec<String> = res
                .into_iter()
                .flat_map(|burst| burst.value)
                .map(|b| String::from_utf8(b).expect("invalid utf8"))
                .collect();
            assert!(
                values.len() >= 3,
                "expected >= 3 items, got {}",
                values.len()
            );
            let nums: Vec<u64> = values.iter().map(|s| s.parse().unwrap()).collect();
            for w in nums.windows(2) {
                assert_eq!(w[1], w[0] + 1, "non-consecutive: {} {}", w[0], w[1]);
            }
            Ok(())
        });
        recv_node
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(1)))
            .unwrap();

        let output = child.wait_with_output().expect("failed to wait on python");
        assert_python_ok(&output, "python_pub_rust_sub_direct");
    }

    #[cfg(feature = "zmq-cross-lang-etcd-test")]
    mod etcd {
        use super::*;
        use crate::adapters::etcd::EtcdConnection;
        use crate::adapters::zmq::EtcdRegistry;
        use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};

        const ETCD_PORT: u16 = 2379;
        const ETCD_IMAGE: &str = "gcr.io/etcd-development/etcd";
        const ETCD_TAG: &str = "v3.5.0";

        fn start_etcd() -> anyhow::Result<(impl Drop, String)> {
            let container = GenericImage::new(ETCD_IMAGE, ETCD_TAG)
                .with_wait_for(WaitFor::message_on_stderr(
                    "now serving peer/client/metrics",
                ))
                .with_env_var("ETCD_LISTEN_CLIENT_URLS", "http://0.0.0.0:2379")
                .with_env_var("ETCD_ADVERTISE_CLIENT_URLS", "http://0.0.0.0:2379")
                .start()?;
            let port = container.get_host_port_ipv4(ETCD_PORT)?;
            let endpoint = format!("http://127.0.0.1:{port}");
            Ok((container, endpoint))
        }

        #[test]
        fn zmq_rust_pub_python_sub_etcd() {
            if !python_available() {
                eprintln!("SKIP: wingfoil python package not installed");
                return;
            }
            _ = env_logger::try_init();
            let (_container, endpoint) = start_etcd().unwrap();
            let port = 5582u16;
            let service = "cross-lang/rust-pub";

            // Rust publisher with etcd registration.
            let ep = endpoint.clone();
            let pub_handle = std::thread::spawn(move || {
                let conn = EtcdConnection::new(ep);
                ticker(Duration::from_millis(50))
                    .count()
                    .map(|n: u64| format!("{n}").into_bytes())
                    .zmq_pub(port, (service, EtcdRegistry::new(conn)))
                    .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)))
            });

            // Wait for publisher to register in etcd.
            std::thread::sleep(Duration::from_millis(800));

            let script = format!(
                r#"
import wingfoil as wf
items = []
data, _status = wf.zmq_sub_etcd("{service}", "{endpoint}")
data.inspect(lambda msgs: items.extend(msgs)).run(realtime=True, duration=1.0)
assert len(items) >= 3, f"expected >= 3 items, got {{len(items)}}"
nums = [int(b) for b in items]
for a, b in zip(nums, nums[1:]):
    assert b == a + 1, f"non-consecutive: {{a}}, {{b}}"
"#
            );
            let output = run_python(&script);
            assert_python_ok(&output, "rust_pub_python_sub_etcd");
            pub_handle.join().unwrap().unwrap();
        }

        #[test]
        fn zmq_python_pub_rust_sub_etcd() {
            if !python_available() {
                eprintln!("SKIP: wingfoil python package not installed");
                return;
            }
            _ = env_logger::try_init();
            let (_container, endpoint) = start_etcd().unwrap();
            let port = 5583u16;
            let service = "cross-lang/python-pub";

            // Python publisher with etcd registration.
            let script = format!(
                r#"
import wingfoil as wf
(
    wf.ticker(0.05)
    .count()
    .map(lambda n: str(n).encode())
    .zmq_pub_etcd("{service}", {port}, "{endpoint}")
    .run(realtime=True, duration=3.0)
)
"#
            );
            let child = Command::new("python3")
                .args(["-c", &script])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("failed to spawn python publisher");

            // Wait for Python publisher to register in etcd.
            std::thread::sleep(Duration::from_millis(800));

            // Rust subscriber via etcd.
            let conn = EtcdConnection::new(&endpoint);
            let (data, _status) = zmq_sub::<Vec<u8>>((service, EtcdRegistry::new(conn)))
                .expect("zmq_sub_etcd failed");
            let recv_node = data.collect().finally(|res, _| {
                let values: Vec<String> = res
                    .into_iter()
                    .flat_map(|burst| burst.value)
                    .map(|b| String::from_utf8(b).expect("invalid utf8"))
                    .collect();
                assert!(
                    values.len() >= 3,
                    "expected >= 3 items, got {}",
                    values.len()
                );
                let nums: Vec<u64> = values.iter().map(|s| s.parse().unwrap()).collect();
                for w in nums.windows(2) {
                    assert_eq!(w[1], w[0] + 1, "non-consecutive: {} {}", w[0], w[1]);
                }
                Ok(())
            });
            recv_node
                .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(1)))
                .unwrap();

            let output = child.wait_with_output().expect("failed to wait on python");
            assert_python_ok(&output, "python_pub_rust_sub_etcd");
        }
    }
}

// --- etcd discovery integration tests (ports 5596–5610) ---

#[cfg(feature = "zmq-etcd-integration-test")]
mod etcd_tests {
    use super::*;
    use crate::adapters::etcd::EtcdConnection;
    use crate::adapters::zmq::EtcdRegistry;
    use etcd_client::Client;
    use testcontainers::{GenericImage, ImageExt, core::WaitFor, runners::SyncRunner};

    const ETCD_PORT: u16 = 2379;
    const ETCD_IMAGE: &str = "gcr.io/etcd-development/etcd";
    const ETCD_TAG: &str = "v3.5.0";

    fn start_etcd() -> anyhow::Result<(impl Drop, EtcdConnection)> {
        let container = GenericImage::new(ETCD_IMAGE, ETCD_TAG)
            .with_wait_for(WaitFor::message_on_stderr(
                "now serving peer/client/metrics",
            ))
            .with_env_var("ETCD_LISTEN_CLIENT_URLS", "http://0.0.0.0:2379")
            .with_env_var("ETCD_ADVERTISE_CLIENT_URLS", "http://0.0.0.0:2379")
            .start()?;
        let port = container.get_host_port_ipv4(ETCD_PORT)?;
        let endpoint = format!("http://127.0.0.1:{port}");
        Ok((container, EtcdConnection::new(endpoint)))
    }

    fn read_key(conn: &EtcdConnection, key: &str) -> anyhow::Result<Option<String>> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(async {
            let mut client = Client::connect(&conn.endpoints, None).await?;
            let resp = client.get(key, None).await?;
            Ok(resp
                .kvs()
                .first()
                .and_then(|kv| kv.value_str().ok())
                .map(|s| s.to_string()))
        })
    }

    #[test]
    fn zmq_sub_etcd_no_etcd_returns_error() {
        // No container — connection refused should propagate.
        let conn = EtcdConnection::new("http://127.0.0.1:59999");
        let result = zmq_sub::<u64>(("anything", EtcdRegistry::new(conn)));
        assert!(result.is_err(), "expected error when etcd is unreachable");
    }

    #[test]
    fn zmq_sub_etcd_name_not_found() {
        let (_container, conn) = start_etcd().unwrap();
        let result = zmq_sub::<u64>(("nonexistent-key", EtcdRegistry::new(conn)));
        assert!(result.is_err(), "expected error for absent key");
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("no publisher named"),
            "unexpected error message: {msg}"
        );
    }

    #[test]
    fn zmq_pub_etcd_registers_address() {
        _ = env_logger::try_init();
        let (_container, conn) = start_etcd().unwrap();
        let port = 5596u16;

        let conn_clone = conn.clone();
        let handle = std::thread::spawn(move || {
            ticker(Duration::from_millis(50))
                .count()
                .zmq_pub(port, ("etcd-quotes", EtcdRegistry::new(conn_clone)))
                .run(
                    RunMode::RealTime,
                    RunFor::Duration(Duration::from_millis(400)),
                )
        });

        std::thread::sleep(Duration::from_millis(200));
        let val = read_key(&conn, "etcd-quotes").unwrap();
        assert!(val.is_some(), "key not written to etcd");
        assert!(
            val.unwrap().contains("5596"),
            "address should contain port 5596"
        );

        handle.join().unwrap().unwrap();
    }

    #[test]
    fn zmq_sub_etcd_end_to_end() {
        _ = env_logger::try_init();
        let (_container, conn) = start_etcd().unwrap();
        let port = 5597u16;
        let run_for = RunFor::Duration(Duration::from_millis(700));

        let conn_pub = conn.clone();
        std::thread::spawn(move || {
            ticker(Duration::from_millis(50))
                .count()
                .zmq_pub(port, ("etcd-data", EtcdRegistry::new(conn_pub)))
                .run(RunMode::RealTime, run_for)
        });

        std::thread::sleep(Duration::from_millis(300));

        let (data, _status) = zmq_sub::<u64>(("etcd-data", EtcdRegistry::new(conn))).unwrap();
        let recv_node = data.collect().finally(|res, _| {
            let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
            assert!(!values.is_empty(), "no data received via etcd discovery");
            Ok(())
        });
        recv_node.run(RunMode::RealTime, run_for).unwrap();
    }

    #[test]
    fn zmq_pub_etcd_lease_revoked_on_stop() {
        _ = env_logger::try_init();
        let (_container, conn) = start_etcd().unwrap();
        let port = 5598u16;

        let conn_clone = conn.clone();
        let handle = std::thread::spawn(move || {
            ticker(Duration::from_millis(50))
                .count()
                .zmq_pub(port, ("etcd-lease-key", EtcdRegistry::new(conn_clone)))
                .run(
                    RunMode::RealTime,
                    RunFor::Duration(Duration::from_millis(200)),
                )
        });
        handle.join().unwrap().unwrap();

        // Give etcd a moment to process the revoke.
        std::thread::sleep(Duration::from_millis(200));
        let val = read_key(&conn, "etcd-lease-key").unwrap();
        assert!(
            val.is_none(),
            "key should be gone after publisher stop, got: {val:?}"
        );
    }

    #[test]
    fn zmq_pub_etcd_historical_mode_fails() {
        use crate::NanoTime;
        let conn = EtcdConnection::new("http://127.0.0.1:59999");
        let result = ticker(Duration::from_millis(10))
            .count()
            .zmq_pub(5599, ("test-hist", EtcdRegistry::new(conn)))
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        let err = result.expect_err("expected historical mode to fail");
        assert!(
            format!("{err:?}").contains("real-time"),
            "expected error to mention real-time"
        );
    }
}
