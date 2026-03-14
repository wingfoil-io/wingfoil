use std::marker::PhantomData;
use std::rc::Rc;

use crate::channel::{ChannelSender, Message};
use crate::{
    Burst, Element, GraphState, IntoNode, IntoStream, MapFilterStream, MutableNode, Node,
    ReceiverStream, RunMode, Stream, UpStreams,
};
use derive_new::new;
use serde::Serialize;
use serde::de::DeserializeOwned;
use zmq;

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
        let socket = self.connect()?;
        channel_sender.send_message(Message::RealtimeValue(ZmqEvent::Status(
            ZmqStatus::Connected,
        )))?;
        let mut done = false;
        while !done {
            let msg: Message<T> = self.recv(&socket)?;
            match msg {
                Message::RealtimeValue(v) => {
                    channel_sender.send_message(Message::RealtimeValue(ZmqEvent::Data(v)))?;
                }
                Message::EndOfStream => {
                    channel_sender.send_message(Message::RealtimeValue(ZmqEvent::Status(
                        ZmqStatus::Disconnected,
                    )))?;
                    channel_sender.send_message(Message::EndOfStream)?;
                    done = true;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn connect(&self) -> anyhow::Result<zmq::Socket> {
        let context = zmq::Context::new();
        let socket = context.socket(zmq::SUB)?;
        socket.connect(&self.address)?;
        socket.set_subscribe("".as_bytes())?;
        Ok(socket)
    }

    fn recv(&self, socket: &zmq::Socket) -> anyhow::Result<Message<T>> {
        let res = socket.recv_bytes(0)?;
        let msg = bincode::deserialize(&res)?;
        Ok(msg)
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

#[derive(new)]
struct ZeroMqSenderNode<T: Element + Send + Serialize> {
    src: Rc<dyn Stream<T>>,
    port: u16,
    bind_address: String,
    #[new(default)]
    socket: Option<zmq::Socket>,
}

const FLAGS: i32 = 0;

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

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.src.clone().as_node()], vec![])
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
}

impl<T: Element + Send + Serialize> ZeroMqPub<T> for Rc<dyn Stream<T>> {
    fn zmq_pub(&self, port: u16) -> Rc<dyn Node> {
        ZeroMqSenderNode::new(self.clone(), port, "127.0.0.1".to_string()).into_node()
    }

    fn zmq_pub_on(&self, address: &str, port: u16) -> Rc<dyn Node> {
        ZeroMqSenderNode::new(self.clone(), port, address.to_string()).into_node()
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::zmq::{ZeroMqPub, ZmqStatus, zmq_sub};
    use crate::{Graph, Node, NodeOperators, StreamOperators};
    use crate::{RunFor, RunMode, ticker};
    use log::Level::Info;
    use std::rc::Rc;
    use std::time::Duration;

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
}
