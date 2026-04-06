use derive_more::Debug;
use derive_new::new;
#[cfg(feature = "async")]
use futures_util::StreamExt;
#[cfg(feature = "async")]
use std::boxed::Box;
use std::option::Option;
#[cfg(feature = "async")]
use std::pin::Pin;

use super::message::Message;
#[cfg(feature = "async")]
use super::message::ReceiverMessageSource;
use super::{SendNodeError, SendResult};
use crate::graph::{GraphState, ReadyNotifier, RunMode};
use crate::queue::ValueAt;
#[cfg(feature = "async")]
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
        match self.kanal_receiver.try_recv() {
            Ok(msg) => msg,
            Err(_) => Some(Message::EndOfStream), // channel closed by sender
        }
    }
    pub fn recv(&self) -> Message<T> {
        self.kanal_receiver.recv().unwrap_or(Message::EndOfStream)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{GraphState, RunFor, RunMode};
    use crate::time::NanoTime;

    fn historical_state() -> GraphState {
        GraphState::new(
            RunMode::HistoricalFrom(NanoTime::ZERO),
            RunFor::Cycles(1),
            NanoTime::ZERO,
        )
    }

    fn realtime_state() -> GraphState {
        GraphState::new(RunMode::RealTime, RunFor::Cycles(1), NanoTime::ZERO)
    }

    #[test]
    fn channel_pair_creates_working_channel() {
        let (tx, rx) = channel_pair::<u64>(None);
        tx.send_message(Message::RealtimeValue(42)).unwrap();
        assert_eq!(rx.try_recv(), Some(Message::RealtimeValue(42)));
    }

    #[test]
    fn try_recv_returns_none_on_empty() {
        let (_tx, rx) = channel_pair::<u64>(None);
        assert_eq!(rx.try_recv(), None);
    }

    #[test]
    fn try_recv_returns_end_of_stream_when_closed() {
        let (tx, rx) = channel_pair::<u64>(None);
        drop(tx);
        assert_eq!(rx.try_recv(), Some(Message::EndOfStream));
    }

    #[test]
    fn recv_returns_end_of_stream_when_sender_dropped() {
        let (tx, rx) = channel_pair::<u64>(None);
        drop(tx);
        assert_eq!(rx.recv(), Message::EndOfStream);
    }

    #[test]
    fn send_in_historical_mode_wraps_as_historical_value() {
        let state = historical_state();
        let (tx, rx) = channel_pair::<u64>(None);
        tx.send(&state, 10).unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::HistoricalValue(_)));
    }

    #[test]
    fn send_in_realtime_mode_wraps_as_realtime_value() {
        let state = realtime_state();
        let (tx, rx) = channel_pair::<u64>(None);
        tx.send(&state, 10).unwrap();
        let msg = rx.try_recv().unwrap();
        assert_eq!(msg, Message::RealtimeValue(10));
    }

    #[test]
    fn send_checkpoint() {
        let state = historical_state();
        let (tx, rx) = channel_pair::<u64>(None);
        tx.send_checkpoint(&state).unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::CheckPoint(_)));
    }

    #[test]
    fn send_historical_batch() {
        let (tx, rx) = channel_pair::<u64>(None);
        let batch = vec![ValueAt::new(1, NanoTime::new(1))];
        tx.send_historical_batch(batch).unwrap();
        let msg = rx.try_recv().unwrap();
        assert!(matches!(msg, Message::HistoricalBatch(_)));
    }

    #[test]
    fn close_sends_end_of_stream_and_idempotent() {
        let (mut tx, rx) = channel_pair::<u64>(None);
        tx.close().unwrap();
        assert_eq!(rx.try_recv(), Some(Message::EndOfStream));
        // second close is a no-op
        tx.close().unwrap();
    }

    #[test]
    fn send_after_close_returns_error() {
        let (mut tx, _rx) = channel_pair::<u64>(None);
        tx.close().unwrap();
        let state = realtime_state();
        assert!(tx.send(&state, 1).is_err());
    }
}

#[cfg(feature = "async")]
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

    #[allow(dead_code)]
    pub fn send_historical_batch(&self, batch: Vec<ValueAt<T>>) -> SendResult {
        let message = Message::HistoricalBatch(batch.into_boxed_slice());
        self.send_message(message)
    }

    pub fn close(&mut self) -> SendResult {
        // check if already closed
        if self.kanal_sender.is_none() {
            return Ok(());
        }
        // Ignore errors when sending EndOfStream - if the receiver is already
        // dropped, the channel is effectively closed anyway
        let _ = self.send_message(Message::EndOfStream);
        self.kanal_sender = None;
        Ok(())
    }

    #[cfg(feature = "async")]
    pub fn into_async(self) -> AsyncChannelSender<T> {
        let ChannelSender {
            mut kanal_sender,
            ready_notifier,
        } = self;
        let kanal_sender = kanal_sender.take().unwrap();
        AsyncChannelSender::new(kanal_sender, ready_notifier)
    }
}

#[cfg(feature = "async")]
#[derive(Debug)]
pub(crate) struct AsyncChannelSender<T: Element + Send> {
    kanal_sender: Option<kanal::AsyncSender<Message<T>>>,
    ready_notifier: Option<ReadyNotifier>,
}

#[cfg(feature = "async")]
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

    #[allow(dead_code)]
    pub async fn send_historical_batch(&self, batch: Vec<ValueAt<T>>) {
        let message = Message::HistoricalBatch(batch.into_boxed_slice());
        self.send_message(message).await;
    }

    pub async fn close(&mut self) {
        let message = Message::EndOfStream;
        self.send_message(message).await;
        self.kanal_sender = None;
    }
}
