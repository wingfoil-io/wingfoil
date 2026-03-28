use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::adapters::zmq_seed;
use crate::channel::{ChannelSender, Message};
use crate::types::*;
use crate::{
    Burst, GraphState, IntoNode, IntoStream, MapFilterStream, Node, ReceiverStream, RunMode, Stream,
};
use derive_new::new;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zmq;

pub use crate::adapters::zmq_seed::SeedHandle;
pub use crate::adapters::zmq_seed::start_seed;

static MONITOR_ID: AtomicUsize = AtomicUsize::new(0);
const ZMQ_EVENT_CONNECTED: u16 = 0x0001;
const ZMQ_EVENT_DISCONNECTED: u16 = 0x0200;

#[derive(Debug, Clone, PartialEq, Default)]
pub enum ZmqStatus {
    Connected,
    #[default]
    Disconnected,
}

#[derive(Debug, Clone)]
pub enum ZmqEvent<T> {
    Data(T),
    Status(ZmqStatus),
}

impl<T: Default> Default for ZmqEvent<T> {
    fn default() -> Self {
        ZmqEvent::Data(T::default())
    }
}

#[derive(new)]
struct ZeroMqSubscriber<T: Element + Send> {
    address: String,
    _phantom: PhantomData<T>,
}

impl<T: Element + Send + DeserializeOwned> ZeroMqSubscriber<T> {
    fn run(&self, channel_sender: ChannelSender<ZmqEvent<T>>) -> anyhow::Result<()> {
        let context = zmq::Context::new();
        let socket = context.socket(zmq::SUB)?;
        socket.connect(&self.address)?;
        socket.set_subscribe("".as_bytes())?;

        let monitor_id = MONITOR_ID.fetch_add(1, Ordering::Relaxed);
        let monitor_addr = format!("inproc://zmq-sub-monitor-{monitor_id}");
        socket.monitor(
            &monitor_addr,
            (ZMQ_EVENT_CONNECTED | ZMQ_EVENT_DISCONNECTED) as i32,
        )?;
        let monitor = context.socket(zmq::PAIR)?;
        monitor.connect(&monitor_addr)?;

        loop {
            let mut items = [
                socket.as_poll_item(zmq::POLLIN),
                monitor.as_poll_item(zmq::POLLIN),
            ];
            zmq::poll(&mut items, -1)?;

            if items[1].is_readable() {
                let event_frame = monitor.recv_msg(0)?;
                while monitor.get_rcvmore()? {
                    monitor.recv_msg(0)?;
                }
                if event_frame.len() >= 2 {
                    let event_id = u16::from_le_bytes([event_frame[0], event_frame[1]]);
                    match event_id {
                        ZMQ_EVENT_CONNECTED => {
                            channel_sender.send_message(Message::RealtimeValue(
                                ZmqEvent::Status(ZmqStatus::Connected),
                            ))?;
                        }
                        ZMQ_EVENT_DISCONNECTED => {
                            channel_sender.send_message(Message::RealtimeValue(
                                ZmqEvent::Status(ZmqStatus::Disconnected),
                            ))?;
                        }
                        _ => {}
                    }
                }
            }

            if items[0].is_readable() {
                let res = socket.recv_bytes(0)?;
                let msg: Message<T> = bincode::deserialize(&res)
                    .unwrap_or_else(|err| Message::Error(std::sync::Arc::new(err.into())));
                match msg {
                    Message::RealtimeValue(v) => {
                        channel_sender.send_message(Message::RealtimeValue(ZmqEvent::Data(v)))?;
                    }
                    Message::EndOfStream => {
                        channel_sender.send_message(Message::RealtimeValue(ZmqEvent::Status(
                            ZmqStatus::Disconnected,
                        )))?;
                        channel_sender.send_message(Message::EndOfStream)?;
                        return Ok(());
                    }
                    Message::Error(err) => {
                        channel_sender.send_message(Message::Error(err))?;
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

pub fn zmq_sub<T: Element + Send + DeserializeOwned>(
    address: &str,
) -> (Rc<dyn Stream<Burst<T>>>, Rc<dyn Stream<ZmqStatus>>) {
    let events: Rc<dyn Stream<Burst<ZmqEvent<T>>>> = {
        let subscriber = ZeroMqSubscriber::new(address.to_string());
        ReceiverStream::new(move |s| subscriber.run(s), true).into_stream()
    };
    let data = MapFilterStream::new(
        events.clone(),
        Box::new(|burst: Burst<ZmqEvent<T>>| {
            let data: Burst<T> = burst
                .into_iter()
                .filter_map(|e| {
                    if let ZmqEvent::Data(v) = e {
                        Some(v)
                    } else {
                        None
                    }
                })
                .collect();
            let ticked = !data.is_empty();
            (data, ticked)
        }),
    )
    .into_stream();
    let status = MapFilterStream::new(
        events,
        Box::new(|burst: Burst<ZmqEvent<T>>| {
            match burst
                .into_iter()
                .filter_map(|e| {
                    if let ZmqEvent::Status(s) = e {
                        Some(s)
                    } else {
                        None
                    }
                })
                .last()
            {
                Some(s) => (s, true),
                None => (ZmqStatus::default(), false),
            }
        }),
    )
    .into_stream();
    (data, status)
}

struct Registration {
    name: String,
    seeds: Vec<String>,
}

struct ZeroMqSenderNode<T: Element + Send + Serialize> {
    src: Rc<dyn Stream<T>>,
    port: u16,
    bind_address: String,
    registration: Option<Registration>,
    socket: Option<zmq::Socket>,
}

impl<T: Element + Send + Serialize> ZeroMqSenderNode<T> {
    fn new(src: Rc<dyn Stream<T>>, port: u16, bind_address: String) -> Self {
        Self {
            src,
            port,
            bind_address,
            registration: None,
            socket: None,
        }
    }

    fn new_named(
        src: Rc<dyn Stream<T>>,
        port: u16,
        bind_address: String,
        name: String,
        seeds: Vec<String>,
    ) -> Self {
        Self {
            src,
            port,
            bind_address,
            registration: Some(Registration { name, seeds }),
            socket: None,
        }
    }
}

const FLAGS: i32 = 0;

#[node(active = [src])]
impl<T: Element + Send + Serialize> MutableNode for ZeroMqSenderNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let value = self.src.peek_value();
        let msg = Message::build(value, state);
        let data = bincode::serialize(&msg)?;
        let sock = self
            .socket
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing socket"))?;
        sock.send(data, FLAGS)?;
        Ok(true)
    }

    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        if state.run_mode() != RunMode::RealTime {
            anyhow::bail!("ZMQ nodes only support real-time mode");
        }
        let context = zmq::Context::new();
        let socket = context.socket(zmq::SocketType::PUB)?;
        let address = format!("tcp://{}:{}", self.bind_address, self.port);
        socket.bind(&address)?;
        self.socket = Some(socket);
        if let Some(reg) = &self.registration {
            zmq_seed::register_with_seeds(&reg.name, &address, &reg.seeds)?;
        }
        Ok(())
    }

    fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        let Some(sock) = self.socket.as_ref() else {
            return Ok(());
        };
        let msg: Message<T> = Message::EndOfStream;
        let data = bincode::serialize(&msg)?;
        sock.send(data, FLAGS)?;
        Ok(())
    }
}

pub trait ZeroMqPub<T: Element + Send> {
    fn zmq_pub(&self, port: u16) -> Rc<dyn Node>;
    fn zmq_pub_on(&self, address: &str, port: u16) -> Rc<dyn Node>;
    /// Publish and register this stream under `name` with the given seeds.
    /// Subscribers can then use [`zmq_sub_discover`] to find it by name.
    /// `port` is bound on `127.0.0.1`; use [`zmq_pub_named_on`](Self::zmq_pub_named_on)
    /// for a routable address in multi-host deployments.
    fn zmq_pub_named(&self, name: &str, port: u16, seeds: &[&str]) -> Rc<dyn Node>;
    /// Like [`zmq_pub_named`](Self::zmq_pub_named) but binds on `address` instead of
    /// `127.0.0.1`. The address must be reachable by subscribing nodes.
    fn zmq_pub_named_on(
        &self,
        name: &str,
        address: &str,
        port: u16,
        seeds: &[&str],
    ) -> Rc<dyn Node>;
}

impl<T: Element + Send + Serialize> ZeroMqPub<T> for Rc<dyn Stream<T>> {
    fn zmq_pub(&self, port: u16) -> Rc<dyn Node> {
        ZeroMqSenderNode::new(self.clone(), port, "127.0.0.1".to_string()).into_node()
    }

    fn zmq_pub_on(&self, address: &str, port: u16) -> Rc<dyn Node> {
        ZeroMqSenderNode::new(self.clone(), port, address.to_string()).into_node()
    }

    fn zmq_pub_named(&self, name: &str, port: u16, seeds: &[&str]) -> Rc<dyn Node> {
        ZeroMqSenderNode::new_named(
            self.clone(),
            port,
            "127.0.0.1".to_string(),
            name.to_string(),
            seeds.iter().map(|s| s.to_string()).collect(),
        )
        .into_node()
    }

    fn zmq_pub_named_on(
        &self,
        name: &str,
        address: &str,
        port: u16,
        seeds: &[&str],
    ) -> Rc<dyn Node> {
        ZeroMqSenderNode::new_named(
            self.clone(),
            port,
            address.to_string(),
            name.to_string(),
            seeds.iter().map(|s| s.to_string()).collect(),
        )
        .into_node()
    }
}

/// Discover a named publisher via seeds and subscribe to it.
///
/// Queries each seed in order until one resolves `name` to an address, then
/// delegates to [`zmq_sub`]. Returns `Err` if no seed can resolve the name.
pub fn zmq_sub_discover<T: Element + Send + DeserializeOwned>(
    name: &str,
    seeds: &[&str],
) -> anyhow::Result<(Rc<dyn Stream<Burst<T>>>, Rc<dyn Stream<ZmqStatus>>)> {
    let address = zmq_seed::query_seeds(name, seeds)?;
    Ok(zmq_sub::<T>(&address))
}

#[cfg(test)]
mod tests {
    use crate::adapters::zmq::{ZeroMqPub, ZmqStatus, zmq_sub, zmq_sub_discover};
    use crate::adapters::zmq_seed::{query_seeds, register_with_seeds, start_seed};
    use crate::{Graph, Node, NodeOperators, StreamOperators};
    use crate::{RunFor, RunMode, ticker};
    use log::Level::Info;
    use std::rc::Rc;
    use std::time::Duration;

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

        let (data, _status) = zmq_sub::<u64>(&format!("tcp://127.0.0.1:{port}"));
        let result = data
            .as_node()
            .run(RunMode::RealTime, RunFor::Duration(Duration::from_secs(3)));

        assert!(
            result.is_err(),
            "expected deserialization error to propagate"
        );
    }

    fn sender(period: Duration, port: u16) -> Rc<dyn Node> {
        ticker(period).count().logged("pub", Info).zmq_pub(port)
    }

    fn sender_with_delay(period: Duration, port: u16) -> Rc<dyn Node> {
        ticker(period)
            .count()
            .delay(Duration::from_millis(200))
            .logged("pub", Info)
            .zmq_pub(port)
    }

    fn receiver(address: &str) -> Rc<dyn Node> {
        let (data, _status) = zmq_sub::<u64>(address);
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

    fn free_local_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("expected to bind ephemeral port")
            .local_addr()
            .expect("expected local addr")
            .port()
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
        // Use an ephemeral port to reduce collisions when tests are run concurrently.
        let port = free_local_port();
        let address = format!("tcp://127.0.0.1:{port}");
        // This test is sensitive to connection establishment timing across threads.
        // Give ZMQ enough time to connect and deliver a few messages reliably.
        let run_for = RunFor::Duration(Duration::from_secs(1));
        let rf_send = run_for;
        let rf_rec = run_for;
        let rec = std::thread::spawn(move || receiver(&address).run(RunMode::RealTime, rf_rec));
        let send = std::thread::spawn(move || {
            sender_with_delay(period, port).run(RunMode::RealTime, rf_send)
        });
        send.join().unwrap().unwrap();
        rec.join().unwrap().unwrap();
    }

    #[test]
    fn zmq_first_message_not_dropped() {
        _ = env_logger::try_init();
        let period = Duration::from_millis(50);
        let port = 5560;
        let address = format!("tcp://127.0.0.1:{port}");
        let run_for = RunFor::Duration(period * 15);
        let (data, _status) = zmq_sub::<u64>(&address);
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
    fn zmq_reports_connected_status() {
        _ = env_logger::try_init();
        let period = Duration::from_millis(50);
        let port = 5561;
        let address = format!("tcp://127.0.0.1:{port}");
        let run_for = RunFor::Duration(period * 10);
        let (data, status) = zmq_sub::<u64>(&address);
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

    // --- Discovery tests (ports 5580–5595) ---

    #[test]
    fn zmq_named_pub_registers_with_seed() {
        _ = env_logger::try_init();
        let seed_addr = "tcp://127.0.0.1:5580";
        let port = 5581u16;

        let _seed = start_seed(seed_addr).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        // Node must be constructed inside the thread (Rc is !Send).
        let handle = std::thread::spawn(move || {
            ticker(Duration::from_millis(50))
                .count()
                .zmq_pub_named("prices", port, &[seed_addr])
                .run(
                    RunMode::RealTime,
                    RunFor::Duration(Duration::from_millis(300)),
                )
        });

        std::thread::sleep(Duration::from_millis(150));
        let addr = query_seeds("prices", &[seed_addr]).unwrap();
        assert_eq!(addr, format!("tcp://127.0.0.1:{port}"));

        handle.join().unwrap().unwrap();
    }

    #[test]
    fn zmq_sub_discover_end_to_end() {
        _ = env_logger::try_init();
        let seed_addr = "tcp://127.0.0.1:5582";
        let port = 5583u16;
        let run_for = RunFor::Duration(Duration::from_millis(600));

        let _seed = start_seed(seed_addr).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        // Publisher: register with seed and publish counts.
        // Node must be constructed inside the thread (Rc is !Send).
        std::thread::spawn(move || {
            ticker(Duration::from_millis(50))
                .count()
                .zmq_pub_named("counts", port, &[seed_addr])
                .run(RunMode::RealTime, run_for)
        });

        // Give the publisher time to start and register.
        std::thread::sleep(Duration::from_millis(150));

        let (data, _status) = zmq_sub_discover::<u64>("counts", &[seed_addr]).unwrap();
        let recv_node = data.collect().finally(|res, _| {
            let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
            assert!(!values.is_empty(), "no data received via discovery");
            Ok(())
        });
        recv_node.run(RunMode::RealTime, run_for).unwrap();
    }

    #[test]
    fn zmq_sub_discover_multiple_seeds_first_down() {
        _ = env_logger::try_init();
        let dead_seed = "tcp://127.0.0.1:5584";
        let live_seed = "tcp://127.0.0.1:5585";

        let _seed = start_seed(live_seed).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        register_with_seeds("widget", "tcp://127.0.0.1:9999", &[live_seed.to_string()]).unwrap();

        // First seed is dead — query_seeds should fall through to live_seed.
        let addr = query_seeds("widget", &[dead_seed, live_seed]);
        assert!(
            addr.is_ok(),
            "expected fallback to live seed, got: {:?}",
            addr
        );
        assert_eq!(addr.unwrap(), "tcp://127.0.0.1:9999");
    }

    #[test]
    fn zmq_sub_discover_no_seed_returns_error() {
        let result = zmq_sub_discover::<u64>("anything", &["tcp://127.0.0.1:5586"]);
        assert!(result.is_err(), "expected error when no seed is running");
    }

    #[test]
    fn zmq_sub_discover_name_not_found() {
        let seed_addr = "tcp://127.0.0.1:5587";
        let _seed = start_seed(seed_addr).unwrap();
        std::thread::sleep(Duration::from_millis(50));

        let result = zmq_sub_discover::<u64>("nonexistent", &[seed_addr]);
        assert!(result.is_err(), "expected error for unregistered name");
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("no publisher named"),
            "unexpected error message"
        );
    }

    #[test]
    fn zmq_named_pub_historical_mode_fails() {
        use crate::NanoTime;
        let result = ticker(Duration::from_millis(10))
            .count()
            .zmq_pub_named("test", 5588, &["tcp://127.0.0.1:5589"])
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever);
        let err = result.expect_err("expected historical mode to fail for named zmq publisher");
        assert!(
            format!("{err:?}").contains("real-time"),
            "expected error to mention real-time"
        );
    }
}
