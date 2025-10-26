

use derive_more::Debug;
use derive_new::new;
use std::option::Option;
use std::pin::Pin;
use std::boxed::Box;

use super::message::{Message, ReceiverMessageSource};
use crate::types::Element;
use crate::graph::{ReadyNotifier, GraphState, RunMode};
use crate::queue::ValueAt;
use crate::time::NanoTime;

use super::ring_buffer::{ProducerEnd, ConsumerEnd, HeapRingBuf};

pub fn channel_pair<T: Element + Send>(ready_notifier: Option<ReadyNotifier>) -> (ChannelSender<T>, ChannelReceiver<T>) {
    let rb = HeapRingBuf::new(1_000_000);
    let (prod, cons) = rb.split();
    let sender = ChannelSender::new(prod, ready_notifier);
    let receiver = ChannelReceiver::new(cons);
    (sender, receiver)
}

#[derive(new, Debug)]
pub(crate) struct ChannelReceiver<T: Element + Send> {
    receiver: ConsumerEnd<Message<T>>,
}

impl <T: Element + Send> ChannelReceiver<T> {
    pub fn try_recv(&mut self) -> Option<Message<T>> {
        self.receiver.pop()
    }
    pub fn recv(&mut self) -> Message<T> {
        self.receiver.pop_blocking()
    }
    pub fn teardown(&self) {
    }
}



impl<T: Element + Send> ReceiverMessageSource<T> for ChannelReceiver<T> {
    fn to_boxed_message_stream(self) -> Pin<Box<dyn futures::Stream<Item = Message<T>> + Send>> {
        let strm = async_stream::stream! {
            let mut receiver = self.receiver;
            loop {
                let msg = receiver.pop_async().await;
                yield msg;
            }                
        };
        Box::pin(strm)
    }
}

#[derive(Debug)]
pub(crate) struct ChannelSender<T: Element + Send> {
    sender: ProducerEnd<Message<T>>,
    ready_notifier: Option<ReadyNotifier>,
}

impl<T: Element + Send> ChannelSender<T> {


    pub fn new(sender: ProducerEnd<Message<T>>, ready_notifier: Option<ReadyNotifier>) -> Self {
        Self {
            sender,
            ready_notifier,
        }
    }

    pub fn set_notifier(&mut self, notifier: ReadyNotifier){
        self.ready_notifier = Some(notifier);
    }

    pub fn send_message(&mut self, message: Message<T>) {
        self.sender.push(message);
        if let Some(notifier) = &self.ready_notifier {
            notifier.notify();
        }
    }

    pub fn send(&mut self, state: &GraphState, value: T) {
        let message = match state.run_mode() {
            RunMode::HistoricalFrom(_) => {
                let value_at = ValueAt::new(value, state.time());
                Message::HistoricalValue(value_at)
            }
            RunMode::RealTime => Message::RealtimeValue(value),
        };
        self.send_message(message);
    }

    pub fn send_checkpoint(&mut self, state: &GraphState) {
        let message = Message::CheckPoint(state.time());
        self.send_message(message);
    }

    pub fn close(&mut self) {
        let message = Message::EndOfStream;
        self.send_message(message);
    }

    pub fn into_async(self) -> AsyncChannelSender<T> {
        let ChannelSender{sender, ready_notifier} = self;
        AsyncChannelSender::new(
            sender,
            ready_notifier
        )

    }
}

#[derive(Debug)]
pub(crate) struct AsyncChannelSender<T: Element + Send> {
    sender: ProducerEnd<Message<T>>,
    ready_notifier: Option<ReadyNotifier>,
}

impl<T: Element + Send> AsyncChannelSender<T> {

    pub fn new(sender: ProducerEnd<Message<T>>, ready_notifier: Option<ReadyNotifier>) -> Self {
        Self {
            sender,
            ready_notifier,
        }
    }

    pub async fn send_message(&mut self, message: Message<T>) {
        self.sender.push(message);
        if let Some(notifier) = &self.ready_notifier {
            notifier.notify();
        }
    }

    #[allow(dead_code)]
    pub async fn send(&mut self, run_mode: RunMode, time: NanoTime, value: T) {
        let message = match run_mode {
            RunMode::HistoricalFrom(_) => {
                let value_at = ValueAt::new(value, time);
                Message::HistoricalValue(value_at)
            }
            RunMode::RealTime => Message::RealtimeValue(value),
        };
        self.send_message(message).await;
    }

    #[allow(dead_code)]
    pub async fn send_checkpoint(&mut self, time: NanoTime) {
        let message = Message::CheckPoint(time);
        self.send_message(message).await;
    }

    pub async fn close(&mut self) {
        let message = Message::EndOfStream;
        self.send_message(message).await;
    }
}



