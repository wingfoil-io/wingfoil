

use std::marker::PhantomData;
use std::rc::Rc;

use derive_new::new;
use serde::de::DeserializeOwned;
use serde::{Serialize};
use tinyvec::TinyVec;
use zmq;
use crate::{Element, GraphState, IntoNode, IntoStream, MutableNode, Node, ReceiverStream, Stream, UpStreams};
use crate::channel::{ChannelSender, Message};

enum ZeroMqStatusEvent {
    Connect,
    Disconnect,
}

enum ZeroMqMessage<T: Element + Send> {
    Message(Message<T>),
    Status(ZeroMqStatusEvent)
}

#[derive(new)]
struct ZeroMqSubscriber<T: Element + Send>{
    address: String,
    _phantom: PhantomData<T>
}



impl <T: Element + Send + DeserializeOwned> ZeroMqSubscriber<T> {

    fn run(&self, channel_sender: ChannelSender<T>) -> anyhow::Result<()> {
        let socket = self.connect()?;
        let mut done = false;
        while !done {
            let msg = self.recv(&socket)?;
            done = matches!(msg, Message::EndOfStream);
            channel_sender.send_message(msg)?;
        };
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




pub fn zmq_rec<T: Element + Send + DeserializeOwned>(address: &str) -> Rc<dyn Stream<TinyVec<[T; 1]>>> {
    let subscriber = ZeroMqSubscriber::new(address.to_string());
    let f = move |channel_sender| {subscriber.run(channel_sender)};
    let receiver_stream = ReceiverStream::new(f);
    receiver_stream.into_stream()    
}

#[derive(new)]
struct ZeroMqSenderNode<T: Element + Send + Serialize> {
    src: Rc<dyn Stream<T>>,
    port: u16,
    #[new(default)]
    socket: Option<zmq::Socket>,
}

const FLAGS: i32 = 0; 

impl <T: Element + Send + Serialize> MutableNode for ZeroMqSenderNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        let value = self.src.peek_value();
        let msg = Message::build(value, state);
        let data = bincode::serialize(&msg)?;
        let sock = self.socket.as_ref().ok_or_else(|| anyhow::anyhow!("missing socket"))?;
        sock.send(data, FLAGS)?;
        Ok(true)
    }

    fn upstreams(&self) -> UpStreams {
        UpStreams::new(vec![self.src.clone().as_node()], vec![])
    }

    fn start(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        let context = zmq::Context::new();
        let socket = context.socket(zmq::SocketType::PUB)?;
        let address = format!("tcp://127.0.0.1:{:}", self.port);
        socket.bind(&address)?;
        self.socket = Some(socket);
        Ok(())
    }

    fn stop(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        let sock = self.socket.as_ref().ok_or_else(|| anyhow::anyhow!("missing socket"))?;
        let msg:Message<T> = Message::EndOfStream;
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

pub trait ZeroMqSend<T: Element + Send> {
    fn zmq_send(&self, port: u16) -> Rc<dyn Node>;
}

impl<T: Element + Send + Serialize> ZeroMqSend<T> for Rc<dyn Stream<T>> {
    fn zmq_send(&self, port: u16) -> Rc<dyn Node> {
        ZeroMqSenderNode::new(self.clone(), port).into_node()
    }
}




#[cfg(test)]
mod tests {
    use std::time::Duration;
    use crate::{RunFor, RunMode, ticker};
    use log::Level::Info;
    use crate::{StreamOperators, NodeOperators, Graph};
    use crate::adapters::zmq::{ZeroMqSend, zmq_rec};

    #[test]
    fn zmq_works() {
        _ = env_logger::try_init();
        let period = Duration::from_millis(50);
        for run_mode in [
            RunMode::RealTime, 
            //RunMode::HistoricalFrom(NanoTime::ZERO)
        ] {
            info!("{run_mode:?}");
            let port = 5556;
            let address = format!("tcp://127.0.0.1:{port}");
            let send = ticker(period)
                .count()
                .logged("pub", Info)
                .zmq_send(port);
            let rec = zmq_rec::<u64>(address.as_str())
                .logged("sub", Info)
                .as_node();
            let nodes = vec![send, rec];
            let run_for = RunFor::Duration(period * 10);
            Graph::new(nodes, run_mode, run_for)
                .print()
                .run()
                .unwrap();
        } 
    }

    #[test]
    fn zmq_really_works() {
        _ = env_logger::try_init();
        let period = Duration::from_millis(10);
        for run_mode in [
            RunMode::RealTime, 
            //RunMode::HistoricalFrom(NanoTime::ZERO)
        ] {
            info!("{run_mode:?}");
            let port = 5557;
            let address = format!("tcp://127.0.0.1:{port}");
            let run_for = RunFor::Duration(period * 5);
            let rm_send = run_mode.clone();
            let rf_send = run_for.clone();
            let rm_rec = run_mode.clone();
            let rf_rec = run_for.clone();
            let rec = std::thread::spawn(move || {
                zmq_rec::<u64>(address.as_str())
                    .logged("sub", Info)
                    .collect()
                    .finally(|x, _| {
                        println!("{:?}", x)
                    })
                    .run(rm_rec, rf_rec)
            });
            let send = std::thread::spawn(move || {
                ticker(period)
                    .count()
                    .logged("pub", Info)
                    .zmq_send(port)
                    .run(rm_send, rf_send)
            });
            send.join().unwrap().unwrap();
            rec.join().unwrap().unwrap();
        }
    }

}



