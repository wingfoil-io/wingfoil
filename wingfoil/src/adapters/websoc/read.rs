use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::channel::{ChannelSender, Message};
use crate::{Burst, Element, IntoStream, MapFilterStream, NanoTime, ReceiverStream, Stream};
use derive_new::new;
use serde::de::DeserializeOwned;

use super::{WebSocketEvent, WebSocketMessage, WebSocketStatus};
use tungstenite::{Message as TungsteniteMessage, connect};

// - **`ReceiverStream`** — when the I/O library is synchronous and poll-based (e.g. `zmq`,
//   `iceoryx2`). Dedicates a real OS thread per subscriber to marshall incoming messages
//   into the graph via a channel. Use this to avoid wrapping every blocking call in
//   `spawn_blocking`.

// ### ReceiverStream pattern (synchronous / poll-based I/O)

// ```rust
// pub fn $ARGUMENTS_sub<T: Element + Send>(address: &str) -> Rc<dyn Stream<Burst<T>>> {
//     let subscriber = Subscriber::new(address.to_string());
//     ReceiverStream::new(
//         move |channel_sender, stop_flag| subscriber.run(channel_sender, stop_flag),
//         true, // real-time only
//     )
//     .into_stream()
// }
// ```

// The callback runs on a dedicated OS thread. Use `stop_flag: Arc<AtomicBool>` to
// cooperatively shut down, and `channel_sender: ChannelSender<T>` to push
// `Message::RealtimeValue(v)`, `Message::EndOfStream`, or `Message::Error(e)`.

#[derive(Clone, new)]
struct WebSocketSubscriber<T: Element + Send> {
    address: String,
    sub_msg: Option<String>,
    heartbeat: Option<Heartbeat>,
    _phantom: PhantomData<T>,
}

#[derive(Clone, new)]
pub struct Heartbeat {
    pub message: TungsteniteMessage,
    pub frequency: u64,
}

/// Returns a [ReceiverStream] that is subscribed to a WebSocket.
impl<T: Element + Send + DeserializeOwned> WebSocketSubscriber<T> {
    fn run(
        &self,
        channel: ChannelSender<WebSocketEvent>,
        stop: Arc<AtomicBool>,
    ) -> anyhow::Result<()> {
        let address = &self.address;
        let mut socket = match connect(address) {
            Ok((soc, res)) => {
                channel.send_message(Message::RealtimeValue(WebSocketEvent::Status(
                    WebSocketStatus::Connected,
                )))?;
                info!("Connected to websocket.");
                info!("HTTP status code: {}", res.status());
                info!("Response headers:");
                for (ref header, value) in res.headers() {
                    info!("- {}: {:?}", header, value);
                }
                soc
            }
            Err(why) => {
                channel.send_message(Message::Error(std::sync::Arc::new(why.into())))?;
                channel.send_message(Message::RealtimeValue(WebSocketEvent::Status(
                    WebSocketStatus::Disconnected,
                )))?;
                return Ok(());
            }
        };

        if self.sub_msg.is_some() {
            let result = socket.send(TungsteniteMessage::Text(
                self.sub_msg.clone().unwrap().into(),
            ));
            match result {
                Err(why) => {
                    channel.send_message(Message::Error(std::sync::Arc::new(why.into())))?
                }
                Ok(_) => {
                    info!("Sent subscription message.");
                }
            };
        }
        let mut interval: NanoTime = Default::default();
        if self.heartbeat.is_some() {
            let start = NanoTime::now();
            info!("Starting heartbeat timer: {}", start);
            info!(
                "Heartbeat every {:?} seconds.",
                self.heartbeat.clone().unwrap().frequency
            );
            interval = start + Duration::from_secs(self.heartbeat.clone().unwrap().frequency);
        }
        loop {
            if stop.load(Ordering::Relaxed) {
                info!("Stopping gracefully, disconnecting from server...");
                socket.close(None)?;
                socket.flush()?;
                info!("Sent close frame.");
                let close_received = socket.read();
                match close_received {
                    Ok(_) => info!("Server received close frame."),
                    Err(why) => {
                        channel.send_message(Message::Error(std::sync::Arc::new(why.into())))?
                    }
                }
                let closed = socket.read();
                match closed {
                    // The server may send further frames before the close
                    // handshake completes; ignore them and keep waiting for
                    // the ConnectionClosed error below.
                    Ok(msg) => info!("Ignoring frame received while closing: {:?}", msg),
                    Err(why) => match why {
                        tungstenite::Error::ConnectionClosed
                        | tungstenite::Error::AlreadyClosed => {
                            channel.send_message(Message::RealtimeValue(
                                WebSocketEvent::Status(WebSocketStatus::Disconnected),
                            ))?;
                            info!("Disconnected.");
                            return Ok(());
                        }
                        _ => {
                            channel.send_message(Message::Error(std::sync::Arc::new(why.into())))?
                        }
                    },
                }
            }
            if self.heartbeat.is_some() && NanoTime::now() >= interval {
                info!("Heartbeat overdue: {:?}", address);
                info!(
                    "Sending heatbeat: {:?}",
                    self.heartbeat.clone().unwrap().message
                );
                let result = socket.send(self.heartbeat.clone().unwrap().message);
                match result {
                    Err(why) => {
                        channel.send_message(Message::Error(std::sync::Arc::new(why.into())))?
                    }
                    Ok(_) => {
                        info!("Sent heartbeat message. {:?}", address);
                    }
                }
                interval = NanoTime::now()
                    + Duration::from_secs(self.heartbeat.clone().unwrap().frequency);
            }
            let raw_msg = socket.read();
            match raw_msg {
                // A read error on a blocking socket is terminal (the connection is
                // gone, or the peer sent something unparseable). Report it and stop
                // rather than looping — `read` would return the same error forever.
                Err(why) => {
                    channel.send_message(Message::Error(std::sync::Arc::new(why.into())))?;
                    channel.send_message(Message::RealtimeValue(WebSocketEvent::Status(
                        WebSocketStatus::Disconnected,
                    )))?;
                    return Ok(());
                }
                Ok(message) => {
                    match message {
                        TungsteniteMessage::Text(msg) => {
                            //info!("{:?}", msg);
                            channel.send_message(Message::RealtimeValue(WebSocketEvent::Data(
                                WebSocketMessage::Text(msg),
                            )))?;
                        }
                        TungsteniteMessage::Binary(msg) => {
                            channel.send_message(Message::RealtimeValue(WebSocketEvent::Data(
                                WebSocketMessage::Binary(msg),
                            )))?;
                        }
                        TungsteniteMessage::Ping(msg) => {
                            // Automatically sends pong with ping payload
                            info!("Received ping: {:?} from {:?}", msg, address);
                            let result = socket.flush();
                            match result {
                                Err(why) => channel.send_message(Message::Error(
                                    std::sync::Arc::new(why.into()),
                                ))?,
                                Ok(_) => {
                                    info!("Sending pong: {:?}", address);
                                    continue;
                                }
                            };
                        }
                        TungsteniteMessage::Pong(msg) => {
                            info!("Received pong: {:?}, {:?}", msg, address);
                        }
                        TungsteniteMessage::Close(msg) => {
                            // Need to test all of this with own server
                            if let Some(close_frame) = msg {
                                // Status codes are defined in RFC 6455 §7.4; they are
                                // logged but not yet acted on (no reconnect logic).
                                info!(
                                    "Received close frame from server, code: {:?}, reason: {:?}.",
                                    close_frame.code, close_frame.reason
                                );
                            } else {
                                info!("Received close frame from server.");
                            }
                            // Need to resend close frame, then flush, then await specific error before safely closing.
                            info!("Queuing client close frame...");
                            let _ = socket.close(None);
                            let result = socket.flush();
                            match result {
                                Ok(msg) => {
                                    info!("OK result: {:?}", msg);
                                    channel.send_message(Message::RealtimeValue(
                                        WebSocketEvent::Status(WebSocketStatus::Disconnected),
                                    ))?;
                                    return Ok(());
                                }
                                Err(why) => match why {
                                    // Err result should be returned when close frame sent by server
                                    // Need to test this
                                    tungstenite::Error::ConnectionClosed => {
                                        channel.send_message(Message::RealtimeValue(
                                            WebSocketEvent::Status(WebSocketStatus::Disconnected),
                                        ))?
                                    }
                                    _ => channel.send_message(Message::Error(
                                        std::sync::Arc::new(why.into()),
                                    ))?,
                                },
                            };
                            return Ok(());
                        }
                        // `read` never yields raw frames; they are only produced by
                        // the low-level frame API, which this adapter does not use.
                        TungsteniteMessage::Frame(frame) => {
                            info!("Ignoring unexpected raw frame: {:?}", frame);
                        }
                    }
                }
            }
        }
    }
}

pub fn websocket_sub<T: Element + Send + DeserializeOwned>(
    address: &str,
    sub_msg: Option<String>,
    heartbeat: Option<Heartbeat>,
) -> (
    Rc<dyn Stream<Burst<WebSocketMessage>>>,
    Rc<dyn Stream<WebSocketStatus>>,
) {
    let events: Rc<dyn Stream<Burst<WebSocketEvent>>> = {
        let subscriber: WebSocketSubscriber<T> =
            WebSocketSubscriber::new(address.to_string(), sub_msg, heartbeat);
        ReceiverStream::new(move |s, stop| subscriber.run(s, stop), true).into_stream()
    };
    let data = MapFilterStream::new(
        events.clone(),
        Box::new(|burst: Burst<WebSocketEvent>| {
            let data: Burst<WebSocketMessage> = burst
                .into_iter()
                .filter_map(|e| {
                    if let WebSocketEvent::Data(v) = e {
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
        Box::new(|burst: Burst<WebSocketEvent>| {
            match burst
                .into_iter()
                .filter_map(|e| {
                    if let WebSocketEvent::Status(s) = e {
                        Some(s)
                    } else {
                        None
                    }
                })
                .last()
            {
                Some(s) => (s, true),
                None => (WebSocketStatus::default(), false),
            }
        }),
    )
    .into_stream();
    (data, status)

    //todo!("Close frame, need automatic reconnect.")
}
