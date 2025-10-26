use async_stream::stream;
use futures_util::{
    SinkExt, Stream, StreamExt,
    stream::{SplitSink, SplitStream},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{marker::PhantomData, time::Duration};
use tokio::select;
use tokio::{net::TcpStream, time::interval};
use tokio_tungstenite::tungstenite::protocol;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::protocol::Message};

type SockSink = SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>;
type SockStream = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

pub trait Responder<T>: Clone
where
    T: DeserializeOwned,
{
    fn respond(&self, parsed_message: &T) -> Response;

    fn parse(&self, raw_message: protocol::Message) -> Result<T, anyhow::Error> {
        match raw_message {
            protocol::Message::Text(bytes) => {
                let parse_result: Result<T, serde_json::Error> = serde_json::from_str(&bytes);
                match parse_result {
                    Ok(parsed) => Ok(parsed),
                    Err(err) => {
                        println!("{bytes:?}");
                        println!("{err:?}");
                        Err(err.into())
                    }
                }
            }
            _ => {
                anyhow::bail!("unsupported message{raw_message:?}")
            }
        }
    }
}

type MethodCall = (String, Option<Value>);
pub type Response = Option<MethodCall>;

pub struct JRPCWriter {
    writer: SockSink,
    message_id: usize,
}

pub fn subscribe_message(channel: String) -> (String, Option<Value>) {
    let params = json!({ "channel": channel });
    ("subscribe".to_string(), Some(params))
}

impl JRPCWriter {
    pub async fn send(&mut self, method: String, params: Option<Value>) {
        let request = json!({
            "id": self.message_id,
            "jsonrpc": "2.0",
            "method": method,
            "params": params.unwrap_or(json!({}))
        })
        .to_string();
        if method != "heartbeat" {
            println!("send >> {request}");
        }
        let message = Message::Text(request.into());
        self.writer.send(message).await.unwrap();
        self.message_id += 1;
    }
    pub async fn subscribe(&mut self, channel: String) {
        let (method, params) = subscribe_message(channel);
        self.send(method, params).await;
    }

    async fn heartbeat(&mut self) {
        self.send("heartbeat".to_string(), None).await;
    }
}

pub struct JRPCSocket<F, T>
where
    F: Responder<T>,
    T: DeserializeOwned,
{
    reader: SockStream,
    writer: JRPCWriter,
    responder: F,
    heartbeat: Duration,
    _phantom: PhantomData<T>,
}

impl<F, T> JRPCSocket<F, T>
where
    F: Responder<T>,
    T: DeserializeOwned,
{
    pub async fn connect(url: &str, responder: F, heartbeat: Option<Duration>) -> Self {
        let (ws_stream, _) = connect_async(url).await.unwrap();
        let (writer, reader) = ws_stream.split();
        let writer = JRPCWriter { writer, message_id: 0 };
        let heartbeat = heartbeat.unwrap_or(Duration::MAX);
        let _phantom = Default::default();
        Self {
            reader,
            writer,
            responder,
            heartbeat,
            _phantom,
        }
    }

    pub async fn subscribe(&mut self, channel: impl ToString) {
        self.writer.subscribe(channel.to_string()).await;
    }

    pub fn stream(mut self) -> impl Stream<Item = Result<T, anyhow::Error>> {
        stream! {
            let mut heartbeat = interval(self.heartbeat);
            loop {
                select! {
                    socket_result = self.reader.next() => {
                        match socket_result {
                            Some(Ok(raw_msg)) => {
                                match self.responder.parse(raw_msg) {
                                    Ok(parsed_msg) => {
                                        if let Some((meth, params)) = self.responder.respond(&parsed_msg) {
                                            self.writer.send(meth, params).await;
                                        }
                                        yield Ok(parsed_msg)
                                    },
                                    Err(err) => {
                                        yield Err(err)
                                    }
                                }
                            }
                            Some(Err(err)) => yield Err(err.into()),
                            None => break,
                        }
                    }
                    _ = heartbeat.tick() => {
                        self.writer.heartbeat().await;
                    }
                }
            }
        }
    }
}
