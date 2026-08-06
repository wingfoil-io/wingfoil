use derive_more::Display;
use derive_new::new;
use log::Level::Info;
use log::info;
use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wingfoil::adapters::websoc::*;
use wingfoil::*;

// id used as unique identifier for messages, 64-bit signed integer format
#[derive(Debug, new, Clone, Deserialize, Serialize, Display)]
#[display("SubscriptionRequest")]
pub struct SubscriptionRequest {
    pub method: String,
    pub params: Vec<String>,
    pub id: u16,
}

#[derive(Debug, new, Clone, Serialize, Deserialize, Display)]
#[display("Message")]
#[serde(untagged)]
pub enum BinanceMessage {
    Book(Book),
    SubscriptionResponse { result: Value, id: u16 },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Display)]
#[display("BookWithSymbol")]
pub struct BookWithSymbol {
    pub stream: String,
    pub data: Book,
}

#[derive(Debug, new, Clone, Default, Deserialize, Serialize, Display)]
#[display("Book")]
#[serde(rename_all = "camelCase")]
pub struct Book {
    pub last_update_id: usize,
    pub bids: Vec<PriceSize>,
    pub asks: Vec<PriceSize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Display)]
#[display("PriceSize")]
pub struct PriceSize {
    #[serde(deserialize_with = "de_float_from_str")]
    pub price: f32,
    #[serde(deserialize_with = "de_float_from_str")]
    pub size: f32,
}

pub fn de_float_from_str<'de, D>(deserializer: D) -> Result<f32, D::Error>
where
    D: Deserializer<'de>,
{
    let str_val = String::deserialize(deserializer)?;
    str_val.parse::<f32>().map_err(de::Error::custom)
}

fn main() {
    env_logger::init();
    let ws_address: &str = "wss://stream.binance.com:9443/ws";
    let sub_msg: String = serde_json::to_string(&SubscriptionRequest::new(
        "SUBSCRIBE".to_string(),
        vec!["btcusdt@depth5".to_string()],
        //vec!["btcusdt@aggTrade".to_string()],
        1,
    ))
    .expect("Error creating json.");
    let (data, _status) = websocket_sub::<Vec<Option<f32>>>(ws_address, Some(sub_msg), None);
    let _ = data
        .map(|burst| {
            burst
                .into_iter()
                .map(|ws_msg| match ws_msg {
                    WebSocketMessage::Text(utf8_bytes) => {
                        let msg: BinanceMessage =
                            serde_json::from_str(&utf8_bytes).expect("Can't parse");
                        match msg {
                            BinanceMessage::Book(book) => {
                                let mid = if !book.bids.is_empty() && !book.asks.is_empty() {
                                    (book.bids[0].price + book.asks[0].price) / 2.0
                                } else {
                                    f32::NAN
                                };
                                info!("Mid: {:?}", mid);
                                Some(mid)
                            }
                            BinanceMessage::SubscriptionResponse { result, id } => {
                                info!("Subscribed: {:?}, {:?}", result, id);
                                None
                            }
                        }
                    }
                    _ => None,
                })
                .collect::<Vec<_>>()
        })
        .logged("mid", Info)
        .run(RunMode::RealTime, RunFor::Cycles(5));
    //.run(RunMode::RealTime, RunFor::Forever);
}

// native-tls
// native-tls-vendored
// rustls-tls-native-roots
// rustls-tls-webpki-roots
