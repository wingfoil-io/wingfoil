use crate::channel::{ChannelReceiver, ChannelSender, Message, NotifierChannelSender};
use crate::*;

use anyhow::anyhow;
use derive_more::Debug;
use derive_new::new;
use std::collections::VecDeque;
use std::option::Option;
use std::rc::Rc;

pub(crate) trait ChannelOperators<T>
where
    T: Element + Send,
{
    #[must_use]
    fn send(
        self: &Rc<Self>,
        sender: ChannelSender<T>,
        trigger: Option<Rc<dyn Node>>,
    ) -> Rc<dyn Node>;
}

impl<T> ChannelOperators<T> for dyn Stream<T>
where
    T: Element + Send,
{
    fn send(
        self: &Rc<Self>,
        sender: ChannelSender<T>,
        trigger: Option<Rc<dyn Node>>,
    ) -> Rc<dyn Node> {
        SenderNode::new(self.clone(), sender, trigger).into_node()
    }
}

#[derive(new)]
pub(crate) struct SenderNode<T: Element + Send> {
    source: Rc<dyn Stream<T>>,
    sender: ChannelSender<T>,
    trigger: Option<Rc<dyn Node>>,
    /// Graph index of `source`, resolved once on the first cycle so the
    /// tick-check avoids an `Rc` clone plus hash-map lookup every tick.
    #[new(default)]
    source_index: Option<usize>,
}

impl<T: Element + Send> MutableNode for SenderNode<T> {
    fn upstreams(&self) -> UpStreams {
        let mut upstreams = vec![self.source.clone().as_node()];
        if let Some(trig) = &self.trigger {
            upstreams.push(trig.clone());
        }
        UpStreams::new(upstreams, Vec::new())
    }

    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        //println!("SenderNode::cycle");
        let source_index = *self.source_index.get_or_insert_with(|| {
            state
                .node_index(self.source.clone().as_node())
                .expect("invariant: channel sender source wired at graph init")
        });
        if state.node_index_ticked(source_index) {
            self.sender.send(state, self.source.peek_value())?;
            Ok(true)
        } else {
            match &self.trigger {
                Some(trig) => {
                    debug_assert!(state.ticked(trig.clone()));
                    self.sender.send_checkpoint(state)?;
                }
                None => {
                    anyhow::bail!("None trigger!");
                }
            }
            Ok(false)
        }
    }

    fn stop(&mut self, _state: &mut GraphState) -> anyhow::Result<()> {
        self.sender.send_message(Message::EndOfStream)?;
        Ok(())
    }
}

#[derive(new, Debug)]
pub struct ChannelReceiverStream<T: Element + Send> {
    receiver: ChannelReceiver<T>,
    #[debug(skip)]
    trigger: Option<Rc<dyn Node>>,
    notifier_channel: Option<NotifierChannelSender>,
    #[new(default)]
    value: Burst<T>,
    #[new(default)]
    finished: bool,
    #[new(default)]
    message_time: Option<NanoTime>,
    #[new(default)]
    queue: VecDeque<ValueAt<Burst<T>>>,
}

// `finished` is only read by `ReceiverStream`, which is itself gated behind the
// zmq/aeron adapters; gate the accessor the same way to avoid a dead-code warning
// in the default build.
#[cfg(any(feature = "zmq", feature = "aeron", feature = "aeron-rs-beta"))]
impl<T: Element + Send> ChannelReceiverStream<T> {
    /// Whether the producer has signalled end-of-stream (i.e. a
    /// [`Message::EndOfStream`] has been received and drained).
    pub(crate) fn finished(&self) -> bool {
        self.finished
    }
}

#[node(output = value: Burst<T>)]
impl<T: Element + Send> MutableNode for ChannelReceiverStream<T> {
    fn upstreams(&self) -> UpStreams {
        let mut ups = Vec::new();
        if let Some(trigger) = &self.trigger {
            ups.push(trigger.clone());
        }
        UpStreams::new(ups, vec![])
    }

    fn cycle(&mut self, state: &mut crate::GraphState) -> anyhow::Result<bool> {
        let mut values: Burst<T> = Burst::new();
        match state.run_mode() {
            RunMode::RealTime => {
                // cycle triggered by notiifcation from sender
                loop {
                    if self.finished {
                        break;
                    } else {
                        match self.receiver.try_recv() {
                            Some(message) => match message {
                                Message::RealtimeValue(value) => {
                                    values.push(value);
                                }
                                Message::HistoricalValue(value_at) => {
                                    values.extend(value_at.value);
                                }
                                Message::EndOfStream => self.finished = true,
                                Message::CheckPoint(_) => {}
                                Message::Error(err) => {
                                    return Err(anyhow!(err));
                                }
                            },
                            None => break,
                        }
                    }
                }
            }
            RunMode::HistoricalFrom(_) => {
                // No notifications from the sender. While we are behind the
                // current engine time we block for the next message; once we
                // have caught up (a message stamped at the current time) we
                // switch to non-blocking reads so that a burst of same-time
                // ticks delivered as separate messages is drained together.
                //
                // We must never *block* for the next message once caught up:
                // an untriggered receiver may be fed one value per engine step
                // (e.g. the graph-map worker), and blocking would deadlock it.
                loop {
                    if self.finished {
                        break;
                    }
                    let mut non_blocking = false;
                    if let Some(t) = self.message_time {
                        if t > state.time() {
                            // Read past the current time; nothing more is due now.
                            break;
                        } else if t == state.time() {
                            if self.trigger.is_some() {
                                // Triggered receivers are driven by the trigger
                                // node: deliver what we have and let the next
                                // trigger tick cycle us again.
                                break;
                            }
                            // Caught up to the current time. Drain any further
                            // same-time messages that are *already* buffered,
                            // but do not block for the next one.
                            non_blocking = true;
                        }
                    }

                    let message = if non_blocking {
                        match self.receiver.try_recv() {
                            Some(message) => message,
                            // Nothing more buffered at the current time.
                            None => break,
                        }
                    } else {
                        // block for message
                        self.receiver.recv()
                    };
                    match message {
                        Message::RealtimeValue(_) => {
                            return Err(anyhow!(
                                "received RealtimeValue but RunMode is Historical"
                            ));
                        }
                        Message::HistoricalValue(value_at) => {
                            if value_at.time < state.time() {
                                return Err(anyhow!(
                                    "received Historical message but with time less than graph time, {} < {}",
                                    value_at.time,
                                    state.time()
                                ));
                            }
                            self.message_time = Some(value_at.time);
                            self.queue.push_back(value_at);
                        }
                        Message::EndOfStream => self.finished = true,
                        Message::CheckPoint(check_point) => {
                            self.message_time = Some(check_point);
                        }
                        Message::Error(err) => {
                            return Err(anyhow!(err));
                        }
                    }
                }
                while let Some(value_at) = self.queue.front() {
                    if value_at.time <= state.time() {
                        // front() returned Some, so pop_front is guaranteed.
                        let popped = self.queue.pop_front().expect("front() just returned Some");
                        values.extend(popped.value);
                    } else {
                        break;
                    }
                }
                match self.queue.front() {
                    Some(head) => state.add_callback(head.time),
                    None => {
                        // No buffered look-ahead. If the stream is still open and
                        // we are self-driven (no trigger), schedule one more wakeup
                        // so the next cycle blocks for the next message; a triggered
                        // or finished receiver is left to wind down. Clearing
                        // message_time makes that next cycle block.
                        if !self.finished && self.trigger.is_none() {
                            state.add_callback(state.time());
                        }
                        self.message_time = None;
                    }
                }
            }
        }
        if !values.is_empty() {
            self.value = values;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        match state.run_mode() {
            RunMode::RealTime => {
                if let Some(chan) = self.notifier_channel.take() {
                    chan.send(state.ready_notifier())
                        .map_err(|e| anyhow::anyhow!(e))?;
                }
            }
            RunMode::HistoricalFrom(time) => {
                if self.trigger.is_none() {
                    state.add_callback(time);
                }
            }
        }
        Ok(())
    }

    fn teardown(&mut self, _: &mut GraphState) -> anyhow::Result<()> {
        self.receiver.teardown();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{Message, channel_pair};
    use crate::queue::ValueAt;
    use std::cell::RefCell;

    /// Regression: a historical-mode receiver fed multiple ticks that share the
    /// same engine time, delivered as separate messages in a single burst, must
    /// surface them all at that time without driving engine time backwards.
    ///
    /// The producer publishes two historical values stamped at t=100 as two
    /// distinct `HistoricalValue` messages (exactly what an upstream burst looks
    /// like once it has been fanned out onto the channel), then ends the stream.
    ///
    /// Previously the drain in `ChannelReceiverStream::cycle` stopped reading
    /// after the first value reached the current engine time and cleared
    /// `message_time`. The graph's strict monotonic clock then advanced to t=101
    /// before the second same-time message was read, so the receiver observed a
    /// message whose timestamp (100) was now in the past and aborted the run with
    /// "received Historical message but with time less than graph time, 100 < 101".
    ///
    /// The fix drains every message already buffered at the current time
    /// (non-blocking) before yielding, so a same-time burst is surfaced as one
    /// burst instead of being split across cycles.
    #[test]
    fn same_time_burst_does_not_break_monotonic_engine_time() {
        let (sender, receiver) = channel_pair::<u64>(None);

        // A burst of two ticks at the *same* engine time, fanned out as separate
        // messages onto the channel.
        sender
            .send_message(Message::HistoricalValue(ValueAt::new(
                burst![1u64],
                NanoTime::new(100),
            )))
            .unwrap();
        sender
            .send_message(Message::HistoricalValue(ValueAt::new(
                burst![2u64],
                NanoTime::new(100),
            )))
            .unwrap();
        sender.send_message(Message::EndOfStream).unwrap();
        // Drop the sending end so `teardown()`'s "wait for sender to close" check
        // sees the channel closed.
        drop(sender);

        let receiver_stream = Rc::new(RefCell::new(ChannelReceiverStream::new(
            receiver, None, None,
        )));
        let collected = receiver_stream.as_stream().collect();

        collected
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Forever)
            .expect("same-time burst must not abort the historical run");

        // Flatten every burst the receiver emitted, pairing each value with the
        // engine time it was delivered at.
        let delivered: Vec<(u64, NanoTime)> = collected
            .peek_value()
            .iter()
            .flat_map(|burst| burst.value.iter().map(|v| (*v, burst.time)))
            .collect();

        // Both values must be delivered, and both at t=100 (their real time).
        assert_eq!(
            delivered,
            vec![(1, NanoTime::new(100)), (2, NanoTime::new(100))],
            "expected both same-time values delivered at t=100, got {delivered:?}"
        );
    }
}
