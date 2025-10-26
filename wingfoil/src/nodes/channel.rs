use crate::channel::{ChannelReceiver, ChannelSender, Message, NotifierChannelSender};
use crate::*;

use derive_more::Debug;
use derive_new::new;
use std::collections::VecDeque;
use std::option::Option;
use std::rc::Rc;
use tinyvec::TinyVec;
use anyhow::anyhow;

pub(crate) trait ChannelOperators<T>
where
    T: Element + Send,
{
    fn send(self: &Rc<Self>, sender: ChannelSender<T>, trigger: Option<Rc<dyn Node>>) -> Rc<dyn Node>;
}

impl<T> ChannelOperators<T> for dyn Stream<T>
where
    T: Element + Send,
{
    fn send(self: &Rc<Self>, sender: ChannelSender<T>, trigger: Option<Rc<dyn Node>>) -> Rc<dyn Node> {
        SenderNode::new(self.clone(), sender, trigger).into_node()
    }
}

#[derive(new)]
pub(crate) struct SenderNode<T: Element + Send> {
    source: Rc<dyn Stream<T>>,
    sender: ChannelSender<T>,
    trigger: Option<Rc<dyn Node>>,
}

impl<T: Element + Send> MutableNode for SenderNode<T> {
    fn cycle(&mut self, state: &mut GraphState) -> bool {
        //println!("SenderNode::cycle");
        if state.ticked(self.source.clone().as_node()) {
            let res = self.sender.send(state, self.source.peek_value());
            if res.is_err() {
                state.terminate(res.map_err(|e| {anyhow!(e)}));
            }
            true
        } else {
            match &self.trigger {
                Some(trig) => {
                    debug_assert!(state.ticked(trig.clone()));
                    let res = self.sender.send_checkpoint(state);
                    if res.is_err() {
                        state.terminate(res.map_err(|e| {anyhow!(e)}));
                    }
                }
                None => {
                    state.terminate(Err(anyhow!("None trigger!")));
                }
            }
            false
        }
    }

    fn upstreams(&self) -> UpStreams {
        let mut upstreams = vec![self.source.clone().as_node()];
        if let Some(trig) = &self.trigger {
            upstreams.push(trig.clone());
        }
        UpStreams::new(upstreams, Vec::new())
    }

    fn stop(&mut self, state: &mut GraphState) {
        let res = self.sender.send_message(Message::EndOfStream);
        if res.is_err() {
            state.terminate(res.map_err(|e| anyhow!(e)));
        }
    }
}

#[derive(new, Debug)]
pub(crate) struct ReceiverStream<T: Element + Send> {
    receiver: ChannelReceiver<T>,
    #[debug(skip)]
    trigger: Option<Rc<dyn Node>>,
    notifier_channel: Option<NotifierChannelSender>,
    #[new(default)]
    value: TinyVec<[T; 1]>,
    #[new(default)]
    finished: bool,
    #[new(default)]
    message_time: Option<NanoTime>,
    #[new(default)]
    queue: VecDeque<ValueAt<T>>,
}

impl<T: Element + Send> MutableNode for ReceiverStream<T> {
    fn cycle(&mut self, state: &mut crate::GraphState) -> bool {
        //println!("ReceiverStream::cycle start {:?}", state.time());
        let mut values: TinyVec<[T; 1]> = TinyVec::new();
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
                                    values.push(value_at.value);
                                }
                                Message::EndOfStream => self.finished = true,
                                Message::CheckPoint(_) => {}
                            },
                            None => break,
                        }
                    }
                }
            }
            RunMode::HistoricalFrom(_) => {
                // no notifications from sender, block until message recieved

                loop {
                    if self.finished {
                        break;
                    }
                    if let Some(t) = self.message_time {
                        if t > state.time() {
                            break;
                        } else if t == state.time() {
                            if self.trigger.is_some() {
                                break;
                            } else {
                                //println!("callback {}", t + 1);
                                state.add_callback(t + 1);
                                break;
                            }
                        }
                    }

                    // block for message
                    //println!("blocking for receive");
                    let message = self.receiver.recv();
                    match message {
                        Message::RealtimeValue(_) => {
                            panic!("received RealtimeValue but RunMode is Historical");
                        }
                        Message::HistoricalValue(value_at) => {
                            if value_at.time < state.time() {
                                panic!(
                                    "recieved Historical message but with time less than graph time, {} < {}",
                                    value_at.time,
                                    state.time()
                                );
                            }
                            self.message_time = Some(value_at.time);
                            self.queue.push_back(value_at);
                        }
                        Message::EndOfStream => self.finished = true,
                        Message::CheckPoint(check_point) => {
                            self.message_time = Some(check_point);
                        }
                    }
                }
                while let Some(value_at) = self.queue.front() {
                    if value_at.time <= state.time() {
                        values.push(self.queue.pop_front().unwrap().value);
                    } else {
                        break;
                    }
                }
                if !self.queue.is_empty() {
                    state.add_callback(self.queue.front().unwrap().time);
                }
            }
        }
        if !values.is_empty() {
            self.value = values;
            true
        } else {
            false
        }
    }

    fn upstreams(&self) -> UpStreams {
        let mut ups = Vec::new();
        if let Some(trigger) = &self.trigger {
            ups.push(trigger.clone());
        }
        UpStreams::new(ups, vec![])
    }

    fn setup(&mut self, state: &mut GraphState) {
        match state.run_mode() {
            RunMode::RealTime => {
                if let Some(chan) = self.notifier_channel.take() {
                    chan.send(state.ready_notifier()).unwrap();
                }
            }
            RunMode::HistoricalFrom(time) => {
                if self.trigger.is_none() {
                    state.add_callback(time);
                }
            }
        }
    }

    fn teardown(&mut self, _: &mut GraphState) {
        self.receiver.teardown();
    }
}

impl<T: Element + Send> StreamPeekRef<TinyVec<[T; 1]>> for ReceiverStream<T> {
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        &self.value
    }
}
