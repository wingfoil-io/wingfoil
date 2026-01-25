use std::marker::PhantomData;
use std::rc::Rc;

use crate::channel::{ChannelSender, Message};
use crate::{
    Element, GraphState, IntoNode, IntoStream, MutableNode, Node, ReceiverStream, RunMode, Stream,
    UpStreams,
};
use derive_new::new;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tinyvec::TinyVec;
use zmq;

#[derive(new)]
struct ZeroMqSubscriber<T: Element + Send> {
    address: String,
    _phantom: PhantomData<T>,
}

impl<T: Element + Send + DeserializeOwned> ZeroMqSubscriber<T> {
    fn run(&self, channel_sender: ChannelSender<T>) -> anyhow::Result<()> {
        let socket = self.connect()?;
        let mut done = false;
        while !done {
            let msg = self.recv(&socket)?;
            done = matches!(msg, Message::EndOfStream);
            channel_sender.send_message(msg)?;
        }
        Ok(())
    }

    fn connect(&self) -> anyhow::Result<zmq::Socket> {
        //     let mut address = self.address.clone();
        //     if self.managed {
        //         status_sender.send(ZmqStatus::ResolvingService);
        //         address = zooman::resolve(&self.address);
        //         status_sender.send(ZmqStatus::ResolvedService);
        //     }
        let context = zmq::Context::new();
        let socket = context.socket(zmq::SUB)?;
        socket.connect(&self.address)?;
        // if self.managed {
        //     socket
        //         .set_rcvtimeo(self.timeout.as_millis() as i32)
        //         .unwrap();
        // }
        socket.set_subscribe("".as_bytes())?;
        //info!("ZeroMqSubscriber: Connected to {}", address);
        //status_sender.send(ZmqStatus::Connected);
        Ok(socket)
    }

    fn recv(&self, socket: &zmq::Socket) -> anyhow::Result<Message<T>> {
        let res = socket.recv_bytes(0)?;
        let msg = bincode::deserialize(&res)?;
        Ok(msg)
        // match res {
        //     Ok(data) => {
        //         let msg = bincode::deserialize(&data)?;
        //         match msg {
        //             // SocketMessage::HeartBeat => {
        //             //     debug!("Received Heartbeat");
        //             // }
        //             // SocketMessage::Payload(val) => {
        //             //     sender.send(val);
        //             // }
        //         }
        //     }
        //     Err(err) => {
        //     }
        // }
    }
}

pub fn zmq_sub<T: Element + Send + DeserializeOwned>(
    address: &str,
) -> Rc<dyn Stream<TinyVec<[T; 1]>>> {
    let subscriber = ZeroMqSubscriber::new(address.to_string());
    let f = move |channel_sender| subscriber.run(channel_sender);
    ReceiverStream::new(f, true).into_stream()
}

#[derive(new)]
struct ZeroMqSenderNode<T: Element + Send + Serialize> {
    src: Rc<dyn Stream<T>>,
    port: u16,
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
        let address = format!("tcp://127.0.0.1:{:}", self.port);
        socket.bind(&address)?;
        self.socket = Some(socket);
        Ok(())
    }

    fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        let sock = self
            .socket
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("missing socket"))?;
        let msg: Message<T> = Message::EndOfStream;
        let data = bincode::serialize(&msg)?;
        sock.send(data, FLAGS)?;
        Ok(())
    }
}

// fn connect(address: String) -> zmq::Socket {
// //     let mut address = self.address.clone();
// //     if self.managed {
// //         status_sender.send(ZmqStatus::ResolvingService);
// //         address = zooman::resolve(&self.address);
// //         status_sender.send(ZmqStatus::ResolvedService);
// //     }
//     let context = zmq::Context::new();
//     let socket = context.socket(zmq::SUB).unwrap();
//     socket.connect(&address).unwrap();
//     // if self.managed {
//     //     socket
//     //         .set_rcvtimeo(self.timeout.as_millis() as i32)
//     //         .unwrap();
//     // }
//     socket.set_subscribe("".as_bytes()).unwrap();
//     //info!("ZeroMqSubscriber: Connected to {}", address);
//     //status_sender.send(ZmqStatus::Connected);
//     socket
// }

// fn listen(&self, sender: ChannelSender<T>, status_sender: ChannelSender<ZmqStatus>) {
//     let mut socket = self.connect("tcp://localhost");
//     loop {
//         let res = socket.recv_bytes(0);
//         match res {
//             Ok(data) => {
//                 let msg: SocketMessage<T> = bincode::deserialize(&data).unwrap();
//                 match msg {
//                     SocketMessage::HeartBeat => {
//                         debug!("Received Heartbeat");
//                         continue;
//                     }
//                     SocketMessage::Payload(val) => {
//                         sender.send(val);
//                     }
//                 }
//             }
//             Err(err) => {
//                 debug!("ZeroMqSubscriber: {:?}", err);
//                 status_sender.send(ZmqStatus::ReceiveError);
//                 if self.managed {
//                     socket = self.connect(&status_sender);
//                 } else {
//                     break;
//                 }
//             }
//         }
//     }
// }

pub trait ZeroMqPub<T: Element + Send> {
    fn zmq_pub(&self, port: u16) -> Rc<dyn Node>;
}

impl<T: Element + Send + Serialize> ZeroMqPub<T> for Rc<dyn Stream<T>> {
    fn zmq_pub(&self, port: u16) -> Rc<dyn Node> {
        ZeroMqSenderNode::new(self.clone(), port).into_node()
    }
}

#[cfg(test)]
mod tests {
    use crate::adapters::zmq::{ZeroMqPub, zmq_sub};
    use crate::{Graph, Node, NodeOperators, StreamOperators};
    use crate::{RunFor, RunMode, ticker};
    use log::Level::Info;
    use std::rc::Rc;
    use std::time::Duration;

    fn sender(period: Duration, port: u16) -> Rc<dyn Node> {
        ticker(period).count().logged("pub", Info).zmq_pub(port)
    }

    fn receiver(address: &str) -> Rc<dyn Node> {
        zmq_sub::<u64>(address)
            .logged("sub", Info)
            .collect()
            .finally(|res, _| {
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
        let rf_send = run_for.clone();
        let rf_rec = run_for.clone();
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
