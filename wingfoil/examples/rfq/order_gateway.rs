use derive_new::new;
use futures::stream::StreamExt;
use std::boxed::Box;
use std::pin::Pin;
use std::rc::Rc;
use tinyvec::TinyVec;

use super::message::Order;

use wingfoil::*;

pub trait OrderGateway {
    fn send<'a>(&self, orders: Rc<dyn Stream<'a, TinyVec<[Order; 1]>> + 'a>) -> Rc<dyn Node<'a> + 'a>;
}

#[derive(new, Clone)]
pub struct RealTimeOrderGateway {}

impl RealTimeOrderGateway {
    async fn consume_orders(mut source: Pin<Box<dyn FutStream<'_, TinyVec<[Order; 1]>>>>) {
        while let Some((_time, _value)) = source.next().await {
            //println!("{time:?}, {value:?}");
        }
    }
}

impl OrderGateway for RealTimeOrderGateway {
    fn send<'a>(&self, orders: Rc<dyn Stream<'a, TinyVec<[Order; 1]>> + 'a>) -> Rc<dyn Node<'a> + 'a> {
        let consumer = Box::new(Self::consume_orders);
        orders.consume_async(consumer)
    }
}
