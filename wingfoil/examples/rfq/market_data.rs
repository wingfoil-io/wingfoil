use arrayvec::ArrayString;
use async_stream::stream;
use derive_new::new;
use futures::StreamExt;
use std::rc::Rc;
use std::time::Duration;
use tinyvec::TinyVec;

use wingfoil::adapters::socket::{JRPCSocket, Responder, Response, subscribe_message};
use wingfoil::*;

use super::environment::Environment;
use super::message::{Channel, Message, Params, RfqData, RfqEvent, RfqId, RfqParams};

pub trait MarketDataProvider {
    fn notifications(&self) -> Rc<dyn Stream<TinyVec<[Params; 1]>>>;
}

pub struct HistoricalMarketDataProvider {
    msgs: Vec<(NanoTime, Params)>,
}

impl HistoricalMarketDataProvider {
    pub fn new(n_msgs: usize) -> Self {
        let mut msgs = Vec::new();
        let mut t = 0 as u64;
        let mut i = 0;
        while t <= n_msgs as u64 {
            let rfq_id = format!("{}", i);
            let rfq_id: RfqId = ArrayString::try_from(rfq_id.as_str()).expect("string too long for RfqId");

            let rfq_data = RfqData { id: rfq_id.clone() };
            let rfq_params = RfqParams {
                channel: Channel::Rfqs,
                event: RfqEvent::Added,
                data: rfq_data,
            };
            let params = Params::Rfq(rfq_params);
            t += 1;
            let item = (NanoTime::from(t), params);
            msgs.push(item);
            let rfq_data = RfqData { id: rfq_id.clone() };
            let rfq_params = RfqParams {
                channel: Channel::Rfqs,
                event: RfqEvent::Removed,
                data: rfq_data,
            };
            let params = Params::Rfq(rfq_params);
            t += 1;
            i += 1;
            let item = (NanoTime::from(t as u64), params);
            msgs.push(item);
        }
        Self { msgs }
    }
}

impl MarketDataProvider for HistoricalMarketDataProvider {
    fn notifications(&self) -> Rc<dyn Stream<TinyVec<[Params; 1]>>> {
        let msgs = self.msgs.clone();
        produce_async(async move || {
            stream! {
                for msg in msgs {
                    yield msg;
                }
            }
        })
    }
}

#[derive(new)]
pub struct RealTimeMarketDataProvider {
    env: Environment,
}

impl MarketDataProvider for RealTimeMarketDataProvider {
    fn notifications(&self) -> Rc<dyn Stream<TinyVec<[Params; 1]>>> {
        let env = self.env.clone();
        produce_async(async move || Self::notifications(env).await)
    }
}

impl RealTimeMarketDataProvider {
    async fn notifications(env: Environment) -> impl futures::Stream<Item = (NanoTime, Params)> + Send {
        println!("{env:?}");
        let url = &env.url();
        let heartbeat = Duration::from_secs(2);
        let subscriber = RfqSubscriber::new();
        let mut sock = JRPCSocket::connect(url, subscriber, Some(heartbeat)).await;
        sock.subscribe(Channel::Rfqs).await;
        sock.stream()
            .filter(|res| {
                // only allow subsciption messages (notifications)
                // to pass
                let pass = match res {
                    Ok(msg) => {
                        matches!(msg, Message::Subscription(_))
                    }
                    _ => false,
                };
                futures::future::ready(pass)
            })
            .map(|res| {
                // stamp with time
                // TODO: handle errors
                let params = match res.unwrap() {
                    Message::Subscription(sub) => sub.params,
                    _ => panic!("should not be reachable"),
                };
                let time = NanoTime::now();
                println!("{time:?} parsed sock message");
                (time, params)
            })
    }
}

#[derive(Clone, new)]
struct RfqSubscriber {}

impl Responder<Message> for RfqSubscriber {
    fn respond(&self, parsed_message: &Message) -> Response {
        match parsed_message {
            Message::Subscription(subs) => match &subs.params {
                Params::Rfq(rfq_params) => {
                    let rfq_id = rfq_params.data.id.as_str();
                    let chan = format!("bbo.{rfq_id}");
                    let msg = subscribe_message(chan);
                    Some(msg)
                }
                _ => None,
            },
            _ => None,
        }
    }
}
