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
    
    // We declare everything outside the match to extend lifetimes
    let env = Environment::Test;
    let rt_market_data = RealTimeMarketDataProvider::new(env);
    let hist_market_data = HistoricalMarketDataProvider::new(n_msgs_historical);
    let gateway = RealTimeOrderGateway::new();
    
    let rt_responder = RfqResponder::new(n_rfqs, rt_market_data, gateway.clone());
    let hist_responder = RfqResponder::new(n_rfqs, hist_market_data, gateway);

    let nodes = match run_mode {
        RunMode::RealTime => {
            rt_responder.build()
        }
        RunMode::HistoricalFrom(_) => {
            hist_responder.build()
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
    fn source<'a>(&self) -> Rc<dyn Stream<'a, TinyVec<[MarketData; 1]>> + 'a> {
        // async is abstracted away
        self.market_data_provider.notifications()
    }

    /// output node
    fn send<'a>(&self, orders: Rc<dyn Stream<'a, TinyVec<[Order; 1]>> + 'a>) -> Rc<dyn Node<'a> + 'a> {
        // async is abstracted away
        self.order_gateway.send(orders)
    }

    /// demuxed sources
    fn sources<'a>(
        &self,
    ) -> (
        Vec<Rc<dyn Stream<'a, TinyVec<[MarketData; 1]>> + 'a>>,
        Overflow<'a, TinyVec<[MarketData; 1]>>,
    ) {
        self.source().demux_it(
            self.n_rfqs,     // max num of current rfqs
            Self::partition, // function pointer
        )
    }

    /// main entry point to build the nodes
    pub fn build<'a>(&'a self) -> Vec<Rc<dyn Node<'a> + 'a>> {
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
    fn rfq_circuit<'a>(
        subcircuit_id: usize,
        market_data: Rc<dyn Stream<'a, TinyVec<[MarketData; 1]>> + 'a>,
    ) -> Rc<dyn Stream<'a, Order> + 'a> {
        let label = format!("subcircuit {} received", subcircuit_id);
        market_data.logged(&label, Info).map(move |_mkt_data_burst| {
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
