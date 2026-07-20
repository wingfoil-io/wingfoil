//! The channel layer: cross-thread / cross-process value transport with a
//! typed [`Message`] envelope, ported onto the Op model (Phase 3).
//!
//! A channel is opened with [`channel`](crate::fluent::SourceOps::channel),
//! which returns a source [`Stream`](crate::fluent::Stream) plus a clonable
//! [`ChannelSender`]. The sender is moved to another thread (or async task);
//! each `send` wakes the realtime kernel, so the graph ticks on arrival —
//! the same waker mechanism as an external source, but carrying the richer
//! envelope so producers can also signal end-of-stream and, crucially,
//! **propagate an error**: a [`Message::Error`] makes the receiver's next
//! cycle fail, aborting the run with context (this is why the channel layer
//! needed fallible `cycle` from Phase 0.1 first).
//!
//! Realtime runs only, like external sources — a deterministic historical
//! replay has no place for wall-clock-arriving messages. Values coalesce
//! latest-wins between cycles (as external does); a lossless variant is the
//! busy-poll `poll` source.

use std::sync::Arc;

use wingfoil::NanoTime;

/// The wire envelope carried between a [`ChannelSender`] and its paired
/// receiver source. Mirrors the classic `channel::Message`, minus the
/// serde/burst machinery that the cross-process adapters (zmq, kafka) will
/// reintroduce when they land.
#[derive(Debug)]
pub enum Message<T> {
    /// A value (realtime, or a historical value at the current time).
    Value(T),
    /// A value stamped with an explicit time (for a future historical
    /// replay receiver; the realtime receiver treats it as [`Message::Value`]).
    ValueAt(T, NanoTime),
    /// A progress marker carrying no value — lets a receiving graph advance
    /// even when this channel ticks less often than its other inputs.
    Checkpoint(NanoTime),
    /// No more messages will be sent; the receiver may shut down cleanly.
    EndOfStream,
    /// An error to propagate into the receiving graph: the receiver's next
    /// cycle returns it, aborting the run with context.
    Error(Arc<anyhow::Error>),
}

impl<T: Clone> Clone for Message<T> {
    fn clone(&self) -> Self {
        match self {
            Message::Value(v) => Message::Value(v.clone()),
            Message::ValueAt(v, t) => Message::ValueAt(v.clone(), *t),
            Message::Checkpoint(t) => Message::Checkpoint(*t),
            Message::EndOfStream => Message::EndOfStream,
            Message::Error(e) => Message::Error(e.clone()),
        }
    }
}

impl<T: PartialEq> PartialEq for Message<T> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Message::Value(a), Message::Value(b)) => a == b,
            (Message::ValueAt(a, ta), Message::ValueAt(b, tb)) => a == b && ta == tb,
            (Message::Checkpoint(a), Message::Checkpoint(b)) => a == b,
            (Message::EndOfStream, Message::EndOfStream) => true,
            // Errors never compare equal (matching the classic envelope).
            _ => false,
        }
    }
}

/// The write end of a [`channel`](crate::fluent::SourceOps::channel).
/// Clone-able and `Send`, so it can be moved to any thread or async task.
/// Every method wakes the paired receiver's kernel.
pub struct ChannelSender<T> {
    tx: std::sync::mpsc::Sender<Message<T>>,
    waker: wingfoil::codegen::KernelWaker,
    index: usize,
}

impl<T> Clone for ChannelSender<T> {
    fn clone(&self) -> Self {
        Self {
            tx: self.tx.clone(),
            waker: self.waker.clone(),
            index: self.index,
        }
    }
}

impl<T> ChannelSender<T> {
    pub(crate) fn new(
        tx: std::sync::mpsc::Sender<Message<T>>,
        waker: wingfoil::codegen::KernelWaker,
        index: usize,
    ) -> Self {
        Self { tx, waker, index }
    }

    fn deliver(&self, message: Message<T>) -> bool {
        self.tx.send(message).is_ok() && self.waker.wake(self.index)
    }

    /// Send a value (realtime) or a value at the current time (historical).
    /// Returns false once the receiver is gone.
    pub fn send(&self, value: T) -> bool {
        self.deliver(Message::Value(value))
    }

    /// Send a value stamped with an explicit graph time. In a **historical**
    /// run the receiver replays it deterministically at `time` on the graph
    /// clock (the timestamped `(NanoTime, T)` shape of classic
    /// `produce_async`); in realtime the timestamp is ignored. Producers
    /// driving a historical replay send timestamped values, then [`close`](Self::close).
    pub fn send_at(&self, value: T, time: NanoTime) -> bool {
        self.deliver(Message::ValueAt(value, time))
    }

    /// Propagate an error into the receiving graph: the receiver's next cycle
    /// fails, aborting the run.
    pub fn send_error(&self, error: anyhow::Error) -> bool {
        self.deliver(Message::Error(Arc::new(error)))
    }

    /// A progress marker with no value.
    pub fn checkpoint(&self, time: NanoTime) -> bool {
        self.deliver(Message::Checkpoint(time))
    }

    /// Signal end-of-stream: no more values will be sent.
    pub fn close(&self) -> bool {
        self.deliver(Message::EndOfStream)
    }
}
