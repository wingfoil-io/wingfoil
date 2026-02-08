use derive_new::new;
use futures::stream::StreamExt;
use std::boxed::Box;
use std::pin::Pin;
use std::rc::Rc;

use super::message::Order;

use wingfoil::*;

pub trait OrderGateway {
    fn send(&self, orders: Rc<dyn Stream<Burst<Order>>>) -> Rc<dyn Node>;
}

#[derive(new)]
pub struct RealTimeOrderGateway {}

impl RealTimeOrderGateway {
    async fn consume_orders(
        mut source: Pin<Box<dyn FutStream<Burst<Order>>>>,
    ) -> anyhow::Result<()> {
        while let Some((_time, _value)) = source.next().await {
            //println!("{time:?}, {value:?}");
        }
        Ok(())
    }
}

impl OrderGateway for RealTimeOrderGateway {
    fn send(&self, orders: Rc<dyn Stream<Burst<Order>>>) -> Rc<dyn Node> {
        let consumer = Box::new(Self::consume_orders);
        orders.consume_async(consumer)
    }
}
