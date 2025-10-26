use derive_new::new;
use futures::stream::StreamExt;
use std::boxed::Box;
use std::pin::Pin;
use std::rc::Rc;
use tinyvec::TinyVec;

use super::message::Order;

use wingfoil::*;

pub trait OrderGateway {
    fn send(&self, orders: Rc<dyn Stream<TinyVec<[Order; 1]>>>) -> Rc<dyn Node>;
}

#[derive(new)]
pub struct RealTimeOrderGateway {}

impl RealTimeOrderGateway {
    async fn consume_orders(mut source: Pin<Box<dyn FutStream<TinyVec<[Order; 1]>>>>) {
        while let Some((_time, _value)) = source.next().await {
            //println!("{time:?}, {value:?}");
        }
    }
}

impl OrderGateway for RealTimeOrderGateway {
    fn send(&self, orders: Rc<dyn Stream<TinyVec<[Order; 1]>>>) -> Rc<dyn Node> {
        let consumer = Box::new(Self::consume_orders);
        orders.consume_async(consumer)
    }
}
