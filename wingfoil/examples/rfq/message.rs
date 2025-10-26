use arrayvec::ArrayString;
use derive_new::new;
use serde::Deserialize;
use strum_macros::Display;
use strum_macros::EnumString;

pub type RfqId = ArrayString<29>;

#[derive(Default, Clone, Debug, new)]
pub struct Order {}

#[derive(Debug, Deserialize, EnumString, Display, Clone, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Rfqs,
    Bbo,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum Message {
    Subscription(Subscription),
    #[allow(unused)]
    Result(Result),
    #[allow(unused)]
    Heartbeat(Heartbeat),
    Empty,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "UPPERCASE")]
pub enum RfqEvent {
    Added,
    Removed,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "lowercase")]
pub enum Method {
    Subscription,
}

#[derive(Debug, Deserialize)]
pub struct Header {
    #[allow(unused)]
    pub id: u64,
    //#[allow(unused)]
    //pub jsonrpc: String,
}

#[derive(Debug, Deserialize)]
pub struct Heartbeat {
    #[allow(unused)]
    #[serde(flatten)]
    header: Header,
}

#[derive(Debug, Deserialize)]
pub struct Subscription {
    #[allow(unused)]
    pub method: Method,
    pub params: Params,
}

// TODO: factor out clone

#[derive(Debug, Deserialize, Clone)]
#[serde(untagged)]
pub enum Params {
    Rfq(RfqParams),
    #[allow(unused)]
    Bbo(BboParams),
    Empty,
}

impl Default for Params {
    fn default() -> Self {
        Self::Empty
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct RfqParams {
    #[allow(unused)]
    pub channel: Channel,
    pub data: RfqData,
    #[allow(unused)]
    pub event: RfqEvent,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BboParams {
    #[allow(unused)]
    pub channel: Channel,
    #[allow(unused)]
    pub data: BboData,
}

#[derive(Debug, Deserialize, Clone)]
pub struct BboData {
    #[allow(unused)]
    pub rfq_id: RfqId,
    //#[allow(unused)]
    //pub mark_price: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct RfqData {
    pub id: RfqId,
}

#[derive(Debug, Deserialize)]
pub struct Result {
    #[allow(unused)]
    #[serde(flatten)]
    pub header: Header,
    #[allow(unused)]
    pub result: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    const RESULT: &str = r#"{"id":0,"jsonrpc":"2.0","result":["rfqs"]}"#;
    const HEARTBEAT: &str = r#"{"id":1,"jsonrpc":"2.0"}"#;
    const RFQ: &str = r#"{
        "jsonrpc":"2.0",
        "method":"subscription",
        "params":{
            "channel":"rfqs",
            "data":{
                "account_name":"key1",
                "base_currency":"BTC",
                "clearing_currency":"BTC",
                "closed_reason":null,
                "counterparties":["AKNA","DSK5","MAT20","MAT21","MAT22","MGNRT","MAT3","MAT4","MAT5","MAT6","MAT7","MAT16","DUSH1","BLK12","DSK3","PULS","BLK2","CELST","RATST","BLKT4","BLK17","BLKT6","ZCPT","DNMST","AMBTS","ARI3","BLNKT","PPT1","BLK1","PULS3","BLK11","BLKT","BLK15","BLK14","BLKT1","PULS2","DSK4","BLK18","PIIC","MT2","BLK16","GLXY","YLYNG","BLKT3","QCPT3","AMM20","QCP2","AMM7","TZT","JAIN","AMM12","AMM6","AMM11","AQML","AMM13","KEV1","DSK2","KTL1","KTL2","DSK6","DSK94","AQML2","FWDE","PDEV","ACTST","AAT42","AAT43","AAT44","PAT","AQA1","ZWF1","PCAPL","FIRM","ZHIR","MERE","JNCL","TESTT","ABRG","PION","BITO","PDGM","MT1","FRDR","AQTL","BLK13","BLK10","AWEE","EHOY","XIE2"],
                "created_at":1751727949856.333,
                "description":"Call  11 Jul 25  105000",
                "expires_at":1751728249856.4429,
                "group_signature":"5a32e30feba347ca48ae7f83303b81f9116f92dc60ffddfa16af750b914e4bda",
                "id":"r_2zScWqozT9xVrxMiX8ilHgP1YOR",
                "is_taker_anonymous":true,
                "kind":"OPTION",
                "label":null,
                "last_updated_at":1751727949856.4429,
                "legs":[{"instrument_id":437172,"instrument_name":"BTC-11JUL25-105000-C","price":null,"product_code":"DO","quantity":"100","ratio":"1","side":"BUY"}],
                "product_codes":["DO"],
                "quantity":"100",
                "quote_currency":"BTC",
                "role":"TAKER",
                "side_layering_limit":1,
                "state":"OPEN",
                "strategy_code":"CL",
                "strategy_description":"DO_BTC-11JUL25-105000-C",
                "taker_desk_name":"JEIT",
                "venue":"DBT"
            },
            "event":"ADDED",
            "meta":{
                "seq_group":1722978353,
                "seq_num":23
            }
        }
    }"#;

    const BBO: &str = r#"{
        "jsonrpc":"2.0",
        "method":"subscription",
        "params":{
            "data":{
                "rfq_id":"r_2zSikWXxZW4JUUmyRJxhkaCWpRN",
                "min_price":"0.0715",
                "max_price":"0.008",
                "mark_price":"0.036",
                "best_bid_price":"0.034",
                "best_ask_price":"0.037",
                "best_bid_amount":"41.0",
                "best_ask_amount":"28.7",
                "created_at":1751731023436,
                "legs":[{
                    "best_ask_amount":"28.7",
                    "best_ask_iv":"38.37",
                    "best_ask_price":"0.0370",
                    "best_bid_amount":"41.0",
                    "best_bid_iv":"30.56",
                    "best_bid_price":"0.0340",
                    "instrument_id":437172,
                    "instrument_name":"BTC-11JUL25-105000-C",
                    "mark_price":"0.0360",
                    "mark_price_iv":"35.92",
                    "greeks":{
                        "delta":"0.75417000",
                        "gamma":"0.00007000",
                        "theta":"-134.52654000",
                        "vega":"42.45695000"
                    }
                }],
                "greeks":{
                    "delta":"0.75417000",
                    "gamma":"0.00007000",
                    "theta":"-134.52654000",
                    "vega":"42.45695000"
                }
            },
            "channel":"bbo",
            "meta":{
                "seq_group":4290116209,
                "seq_num":1
            }
        }
    }"#;

    fn parse(msg: &str) -> Message {
        serde_json::from_str::<Message>(msg).unwrap()
    }

    #[test]
    fn test_deserialize_message() {
        assert!(matches!(parse(HEARTBEAT), Message::Heartbeat(_)));
        assert!(matches!(parse(RESULT), Message::Result(_)));
        match parse(RFQ) {
            Message::Subscription(subs) => {
                match subs.params {
                    Params::Rfq(_) => {
                        // ok
                    }
                    _ => panic!(),
                }
            }
            _ => panic!(),
        };
        match parse(BBO) {
            Message::Subscription(subs) => {
                match subs.params {
                    Params::Bbo(_) => {
                        // ok
                    }
                    _ => panic!(),
                }
            }
            _ => panic!(),
        };
    }
}
