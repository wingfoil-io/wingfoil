use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::registry::{ZmqSubConfig, ZmqSubResolution};
use super::{ZmqEvent, ZmqStatus};
use crate::channel::{ChannelSender, Message};
use crate::{Burst, Element, IntoStream, MapFilterStream, ReceiverStream, Stream};
use derive_new::new;
use serde::de::DeserializeOwned;

static MONITOR_ID: AtomicUsize = AtomicUsize::new(0);
const ZMQ_EVENT_CONNECTED: u16 = 0x0001;
const ZMQ_EVENT_DISCONNECTED: u16 = 0x0200;

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

/// Connect to a ZMQ PUB socket and stream incoming messages.
///
/// Accepts either a direct address or a `(name, registry)` tuple for
/// discovery-based lookup:
///
/// ```ignore
/// // Direct address
/// let (data, status) = zmq_sub::<Vec<u8>>("tcp://localhost:5556")?;
///
/// // Seed-based discovery
/// let (data, status) = zmq_sub::<Vec<u8>>(("quotes", SeedRegistry::new(&["tcp://seed:7777"])))?;
///
/// // etcd-based discovery
/// let (data, status) = zmq_sub::<Vec<u8>>(("quotes", EtcdRegistry::new(conn)))?;
/// ```
///
/// Returns a `(data, status)` pair:
/// - `data` ticks with each burst of received messages
/// - `status` ticks when the connection status changes (`Connected`/`Disconnected`)
pub fn zmq_sub<T: Element + Send + DeserializeOwned>(
    config: impl Into<ZmqSubConfig>,
) -> anyhow::Result<(Rc<dyn Stream<Burst<T>>>, Rc<dyn Stream<ZmqStatus>>)> {
    let address = match config.into().0 {
        ZmqSubResolution::Direct(addr) => addr,
        ZmqSubResolution::Discover(name, reg) => reg.lookup(&name)?,
    };
    Ok(zmq_sub_direct::<T>(&address))
}

fn zmq_sub_direct<T: Element + Send + DeserializeOwned>(
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
