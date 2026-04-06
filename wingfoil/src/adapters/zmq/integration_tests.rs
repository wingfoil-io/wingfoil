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
