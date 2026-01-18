use derive_more::Debug;
use derive_new::new;
use futures_util::StreamExt;
use std::boxed::Box;
use std::option::Option;
use std::pin::Pin;

use super::message::{Message, ReceiverMessageSource};
use super::{SendNodeError, SendResult};
use crate::graph::{GraphState, ReadyNotifier, RunMode};
use crate::queue::ValueAt;
use crate::time::NanoTime;
use crate::types::Element;

use kanal::{Receiver, Sender};

pub fn channel_pair<T: Element + Send>(
    ready_notifier: Option<ReadyNotifier>,
) -> (ChannelSender<T>, ChannelReceiver<T>) {
    let (tx, rx) = kanal::unbounded();
    let sender = ChannelSender::new(tx, ready_notifier);
    let receiver = ChannelReceiver::new(rx);
    (sender, receiver)
}

#[derive(new, Debug)]
pub(crate) struct ChannelReceiver<T: Element + Send> {
    kanal_receiver: Receiver<Message<T>>,
}

impl<T: Element + Send> ChannelReceiver<T> {
    pub fn try_recv(&self) -> Option<Message<T>> {
        self.kanal_receiver.try_recv().unwrap()
    }
    pub fn recv(&self) -> Message<T> {
        self.kanal_receiver.recv().unwrap()
    }
    pub fn teardown(&self) {
        for _ in 0..100 {
            if self.kanal_receiver.sender_count() == 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out waiting for sending end of channel to close");
    }
}

impl<T: Element + Send> ReceiverMessageSource<T> for ChannelReceiver<T> {
    fn to_boxed_message_stream(self) -> Pin<Box<dyn futures::Stream<Item = Message<T>> + Send>> {
        let strm = async_stream::stream! {
            let receiver = self.kanal_receiver.to_async();
            let mut s = receiver.stream();
            while let Some(msg) = s.next().await {
                yield msg;
            }
        };
        Box::pin(strm)
    }
}

#[derive(Debug)]
pub(crate) struct ChannelSender<T: Element + Send> {
    kanal_sender: Option<Sender<Message<T>>>,
    ready_notifier: Option<ReadyNotifier>,
}

impl<T: Element + Send> ChannelSender<T> {
    pub fn new(kanal_sender: Sender<Message<T>>, ready_notifier: Option<ReadyNotifier>) -> Self {
        let kanal_sender = Some(kanal_sender);
        Self {
            kanal_sender,
            ready_notifier,
        }
    }

    pub fn set_notifier(&mut self, notifier: ReadyNotifier) {
        self.ready_notifier = Some(notifier);
    }

    pub fn send_message(&self, message: Message<T>) -> SendResult {
        let sender = self
            .kanal_sender
            .as_ref()
            .ok_or(SendNodeError::ChannelClosed)?;
        sender
            .send(message)
            .map_err(|_| SendNodeError::ChannelClosed)?;
        if let Some(notifier) = &self.ready_notifier {
            notifier
                .notify()
                .map_err(|_| SendNodeError::NotifierClosed)?;
        }
        Ok(())
    }

    pub fn send(&self, state: &GraphState, value: T) -> SendResult {
        let message = match state.run_mode() {
            RunMode::HistoricalFrom(_) => {
                let value_at = ValueAt::new(value, state.time());
                Message::HistoricalValue(value_at)
            }
            RunMode::RealTime => Message::RealtimeValue(value),
        };
        self.send_message(message)
    }

    pub fn send_checkpoint(&self, state: &GraphState) -> SendResult {
        let message = Message::CheckPoint(state.time());
        self.send_message(message)
    }

    pub fn close(&mut self) -> SendResult {
        // check if already close
        if self.kanal_sender.is_none() {
            return Ok(());
        }
        let result = self.send_message(Message::EndOfStream);
        self.kanal_sender = None;
        result
    }

    pub fn into_async(self) -> AsyncChannelSender<T> {
        let ChannelSender {
            mut kanal_sender,
            ready_notifier,
        } = self;
        let kanal_sender = kanal_sender.take().unwrap();
        AsyncChannelSender::new(kanal_sender, ready_notifier)
    }
}

#[derive(Debug)]
pub(crate) struct AsyncChannelSender<T: Element + Send> {
    kanal_sender: Option<kanal::AsyncSender<Message<T>>>,
    ready_notifier: Option<ReadyNotifier>,
}

impl<T: Element + Send> AsyncChannelSender<T> {
    pub fn new(sender: kanal::Sender<Message<T>>, ready_notifier: Option<ReadyNotifier>) -> Self {
        let kanal_sender = Some(sender.to_async());
        Self {
            kanal_sender,
            ready_notifier,
        }
    }

    pub async fn send_message(&self, message: Message<T>) {
        self.kanal_sender
            .as_ref()
            .unwrap()
            .send(message)
            .await
            .unwrap();
        if let Some(notifier) = &self.ready_notifier {
            notifier.notify().unwrap();
        }
    }

    #[allow(dead_code)]
    pub async fn send(&self, run_mode: RunMode, time: NanoTime, value: T) {
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
    pub async fn send_checkpoint(&self, time: NanoTime) {
        let message = Message::CheckPoint(time);
        self.send_message(message).await;
    }

    pub async fn close(&mut self) {
        let message = Message::EndOfStream;
        self.send_message(message).await;
        self.kanal_sender = None;
    }
}
