mod environment;
mod market_data;
mod message;
mod order_gateway;

use environment::Environment;
use market_data::{HistoricalMarketDataProvider, MarketDataProvider, RealTimeMarketDataProvider};
use message::{Order, Params as MarketData, RfqEvent, RfqId};
use order_gateway::{OrderGateway, RealTimeOrderGateway};

use derive_new::new;
use log::Level::Info;
use std::rc::Rc;
use tinyvec::TinyVec;

use wingfoil::*;

fn main() {
    env_logger::init();
    let run_mode = RunMode::HistoricalFrom(NanoTime::ZERO);
    let run_for = RunFor::Forever;
    let n_rfqs = 100; // max number of concurrent rfqs
    let n_msgs_historical = 1_000_000;
    let nodes = match run_mode {
        RunMode::RealTime => {
            let env = Environment::Test;
            let responder = RfqResponder::new(
                n_rfqs,
                RealTimeMarketDataProvider::new(env),
                RealTimeOrderGateway::new(),
            );
            responder.build()
        }
        RunMode::HistoricalFrom(_) => {
            let responder = RfqResponder::new(
                n_rfqs,
                HistoricalMarketDataProvider::new(n_msgs_historical),
                RealTimeOrderGateway::new(),
            );
            responder.build()
        }
    };
    let mut graph = Graph::new(nodes, run_mode, run_for);
    let t0 = std::time::Instant::now();
    graph.run().unwrap();
    match run_mode {
        RunMode::HistoricalFrom(_) => {
            let elapsed = std::time::Instant::now() - t0;
            let avg = elapsed / n_msgs_historical as u32;
            println!("{avg:?}");
        }
        _ => {}
    }
}

/// Builder of circuit that receives MarketData and sends Orders
#[derive(new)]
pub struct RfqResponder<M, O>
where
    M: MarketDataProvider + 'static,
    O: OrderGateway + 'static,
{
    n_rfqs: usize,
    market_data_provider: M,
    order_gateway: O,
}

impl<M, O> RfqResponder<M, O>
where
    M: MarketDataProvider + 'static,
    O: OrderGateway + 'static,
{
    /// input streams
    fn source(&self) -> Rc<dyn Stream<TinyVec<[MarketData; 1]>>> {
        // async is abstracted away
        self.market_data_provider.notifications()
    }

    /// output node
    fn send(&self, orders: Rc<dyn Stream<TinyVec<[Order; 1]>>>) -> Rc<dyn Node> {
        // async is abstracted away
        self.order_gateway.send(orders)
    }

    /// demuxed sources
    fn sources(
        &self,
    ) -> (
        Vec<Rc<dyn Stream<TinyVec<[MarketData; 1]>>>>,
        Overflow<TinyVec<[MarketData; 1]>>,
    ) {
        self.source().demux_it(
            self.n_rfqs,     // max num of current rfqs
            Self::partition, // function pointer
        )
    }

    /// main entry point to build the nodes
    pub fn build(&self) -> Vec<Rc<dyn Node>> {
        let (sources, overflow) = self.sources();
        let order_streams = sources
            .iter()
            .enumerate()
            .map(|(i, strm)| Self::rfq_circuit(i, strm.clone()))
            .collect::<Vec<_>>();
        let orders = self.send(combine(order_streams));
        let overflow = overflow.panic();
        vec![orders, overflow]
    }

    /// The main rfq circuit.  
    /// Each circuit can easily be set up to run on worker a thread.
    fn rfq_circuit(
        subcircuit_id: usize,
        market_data: Rc<dyn Stream<TinyVec<[MarketData; 1]>>>,
    ) -> Rc<dyn Stream<Order>> {
        let label = format!("subcircuit {subcircuit_id} received");
        market_data.logged(&label, Info).map(|_mkt_data_burst| {
            //println!("{:?}", mkt_data_burst.len());
            Order::new()
        })
    }

    /// used to demux source
    fn partition(
        market_data: &MarketData,
    ) -> (
        RfqId,
        DemuxEvent, // {None, Close}
    ) {
        match market_data {
            MarketData::Rfq(rfq) => {
                let id = rfq.data.id.clone();
                let event = match rfq.event {
                    RfqEvent::Removed => DemuxEvent::Close,
                    _ => DemuxEvent::None,
                };
                (id, event)
            }
            MarketData::Bbo(bbo) => {
                let id = bbo.data.rfq_id.clone();
                let event = DemuxEvent::None;
                (id, event)
            }
            _ => {
                panic!("unexpected message type")
            }
        }
    }
}
