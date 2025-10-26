use crate::{Element, GraphState, IntoStream, MutableNode, Node, Stream, StreamOperators, StreamPeekRef, UpStreams};
use derive_more::Debug;
use derive_new::new;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::fmt;
use std::hash::Hash;
use std::rc::Rc;
use tinyvec::TinyVec;

/// A message used to signal that a demuxed child stream
/// can be closed.   Used by [StreamOperators::demux] and [StreamOperators::demux_it]
#[derive(Debug)]
pub enum DemuxEvent {
    None,
    Close,
}

enum DemuxEntry {
    Some(usize),
    Overflow,
}
/// Maintains map from Key k, to output node for demux in order to
/// demux a source.  Used by [StreamOperators::demux_it].
#[derive(Debug)]
pub struct DemuxMap<K>
where
    K: Hash + Eq + PartialEq + fmt::Debug,
{
    inner: Rc<RefCell<DemuxMapInner<K>>>,
}

impl<K> DemuxMap<K>
where
    K: Hash + Eq + PartialEq + fmt::Debug,
{
    pub fn new(size: usize) -> Self {
        let inner = DemuxMapInner::new(size);
        let inner = Rc::new(RefCell::new(inner));
        Self { inner }
    }

    fn get_or_insert(&self, key: K) -> DemuxEntry {
        self.inner.borrow_mut().get_or_insert(key)
    }

    fn release(&self, key: &K) -> DemuxEntry {
        self.inner.borrow_mut().release(key)
    }

    fn size(&self) -> usize {
        self.inner.borrow().size
    }
}

#[derive(Debug)]
struct DemuxMapInner<K>
where
    K: Hash + Eq + PartialEq,
{
    available: HashSet<usize>,
    in_use: HashMap<K, Option<usize>>,
    size: usize,
}

impl<K> DemuxMapInner<K>
where
    K: Hash + Eq + PartialEq + std::fmt::Debug,
{
    fn new(size: usize) -> Self {
        let available = (0..size).collect::<HashSet<usize>>();
        let in_use = Default::default();
        Self {
            available,
            in_use,
            size,
        }
    }

    fn release(&mut self, key: &K) -> DemuxEntry {
        match self.in_use.remove(key) {
            Some(index) => match index {
                Some(ix) => {
                    self.available.insert(ix);
                    DemuxEntry::Some(ix)
                }
                None => DemuxEntry::Overflow,
            },
            None => match self.peek_available() {
                Some(ix) => DemuxEntry::Some(ix),
                None => DemuxEntry::Overflow,
            },
        }
    }

    fn get_or_insert(&mut self, key: K) -> DemuxEntry {
        match self.in_use.get(&key) {
            Some(index) => match index {
                Some(ix) => DemuxEntry::Some(*ix),
                None => DemuxEntry::Overflow,
            },
            None => match self.peek_available() {
                Some(index) => {
                    self.available.take(&index).unwrap();
                    self.in_use.insert(key, Some(index));
                    DemuxEntry::Some(index)
                }
                None => {
                    self.in_use.insert(key, None);
                    DemuxEntry::Overflow
                }
            },
        }
    }

    fn peek_available(&self) -> Option<usize> {
        self.available.iter().next().copied()
    }
}

/// Represents a [Stream] of values that failed to be demuxed because the
/// the demux capacity was exceeded.   Output of [StreamOperators::demux] and
/// [StreamOperators::demux_it].
pub struct Overflow<T: Element>(Rc<RefCell<Option<Rc<dyn Stream<T>>>>>);
impl<T: Element> Overflow<T> {
    pub fn stream(&self) -> Rc<dyn Stream<T>> {
        self.0.borrow().clone().unwrap()
    }
    pub fn panic(&self) -> Rc<dyn Node> {
        self.stream().for_each(move |itm, _| {
            panic!("overflow!\n{itm:?}");
        })
    }
}

pub(crate) fn demux<K, T, F>(
    source: Rc<dyn Stream<T>>,
    map: DemuxMap<K>,
    func: F,
) -> (Vec<Rc<dyn Stream<T>>>, Overflow<T>)
where
    K: Hash + Eq + PartialEq + fmt::Debug + 'static,
    T: Element,
    F: Fn(&T) -> (K, DemuxEvent) + 'static,
{
    let size = map.size();
    let overflow = Rc::new(RefCell::new(None));
    let children = Rc::new(RefCell::new(vec![]));

    let parent = DemuxParent::new(source, func, map, children.clone(), overflow.clone()).into_stream();
    let build_child = || DemuxChild::new(parent.clone()).into_stream();
    let demuxed = (0..size).map(|_| build_child()).collect::<Vec<_>>();
    assert!(overflow.borrow().is_none());
    overflow.borrow_mut().replace(build_child());
    assert!(overflow.borrow().is_some());
    demuxed.iter().for_each(|strm| {
        children.borrow_mut().push(strm.clone());
    });
    let overflow = Overflow(overflow);
    (demuxed, overflow)
}

#[derive(new, Debug)]
struct DemuxParent<T, F, K>
where
    T: Element,
    F: Fn(&T) -> (K, DemuxEvent),
    K: Hash + Eq + PartialEq + std::fmt::Debug,
{
    source: Rc<dyn Stream<T>>,
    func: F,
    #[debug(skip)]
    map: DemuxMap<K>,
    children: Rc<RefCell<Vec<Rc<dyn Stream<T>>>>>,
    overflow_child: Rc<RefCell<Option<Rc<dyn Stream<T>>>>>,
    #[new(default)]
    value: T,
    #[new(default)]
    // map from child node index to its index in the graph
    index_map: Vec<usize>,
    #[new(default)]
    overflow_graph_index: Option<usize>,
}

impl<T, F, K> StreamPeekRef<T> for DemuxParent<T, F, K>
where
    T: Element,
    F: Fn(&T) -> (K, DemuxEvent),
    K: Hash + Eq + PartialEq + std::fmt::Debug,
{
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

impl<T, K, F> MutableNode for DemuxParent<T, F, K>
where
    T: Element,
    F: Fn(&T) -> (K, DemuxEvent),
    K: Hash + Eq + PartialEq + std::fmt::Debug,
{
    fn cycle(&mut self, graph_state: &mut GraphState) -> bool {
        self.value = self.source.peek_value();
        let (key, event) = (self.func)(&self.value);
        let entry = match event {
            DemuxEvent::Close => self.map.release(&key),
            DemuxEvent::None => self.map.get_or_insert(key),
        };
        let graph_index = match entry {
            DemuxEntry::Overflow => self.overflow_graph_index.unwrap(),
            DemuxEntry::Some(index) => self.index_map[index],
        };
        // mark dirty directly instead of ticking
        graph_state.mark_dirty(graph_index);
        false
    }

    fn setup(&mut self, graph_state: &mut GraphState) {
        let mut childes = self.children.borrow_mut();
        let mut node_indexes: Vec<_> = childes
            .drain(..)
            .map(|strm| {
                graph_state.node_index(strm.as_node()).unwrap_or_else(|| {
                    panic!("Failed to resolve graph index of demux child node.  Was it added to the graph?")
                })
            })
            .collect();
        drop(childes);
        node_indexes
            .drain(..)
            .for_each(|node_index| self.index_map.push(node_index));
        let overflow = self.overflow_child.take().unwrap();
        self.overflow_graph_index = graph_state.node_index(overflow.as_node());
        assert!(
            self.overflow_graph_index.is_some(),
            "Failed to resolve graph index of demux overflow node.  Was it added to the graph?"
        );
        assert!(
            !self.index_map.is_empty(),
            "Failed to resolve any children to demux into"
        );
    }

    fn upstreams(&self) -> UpStreams {
        let nodes = vec![self.source.clone().as_node()];
        UpStreams::new(nodes, vec![])
    }
}

#[derive(new)]
struct DemuxChild<T>
where
    T: Element,
{
    source: Rc<dyn Stream<T>>,
    #[new(default)]
    value: T,
}

impl<T> MutableNode for DemuxChild<T>
where
    T: Element,
{
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        self.value = self.source.peek_value();
        true
    }

    fn upstreams(&self) -> UpStreams {
        // source never ticks but use passive wiring anyway
        UpStreams::new(vec![], vec![self.source.clone().as_node()])
    }
}

impl<T> StreamPeekRef<T> for DemuxChild<T>
where
    T: Element,
{
    fn peek_ref(&self) -> &T {
        &self.value
    }
}

/////////////////////////////////////

pub (crate) fn demux_it<K, T, F, I>(
    source: Rc<dyn Stream<I>>,
    map: DemuxMap<K>,
    func: F,
) -> (Vec<Rc<dyn Stream<TinyVec<[T; 1]>>>>, Overflow<TinyVec<[T; 1]>>)
where
    K: Hash + Eq + PartialEq + fmt::Debug + 'static,
    T: Element,
    F: Fn(&T) -> (K, DemuxEvent) + 'static,
    I: IntoIterator<Item = T> + Element,
{
    let size = map.size();
    let overflow = Rc::new(RefCell::new(None));
    let children = Rc::new(RefCell::new(vec![]));
    let value = vec![TinyVec::new(); size + 1];
    let parent = DemuxVecParent::new(source, func, map, children.clone(), overflow.clone(), value).into_stream();
    let build_child = |i| DemuxVecChild::new(i, parent.clone()).into_stream();
    let demuxed = (0..size).map(build_child).collect::<Vec<_>>();
    assert!(overflow.borrow().is_none());
    overflow.borrow_mut().replace(build_child(size));
    assert!(overflow.borrow().is_some());
    demuxed.iter().for_each(|strm| {
        children.borrow_mut().push(strm.clone());
    });
    let overflow = Overflow(overflow);
    (demuxed, overflow)
}

#[derive(new, Debug)]
struct DemuxVecParent<T, F, K, I>
where
    T: Element,
    F: Fn(&T) -> (K, DemuxEvent),
    K: Hash + Eq + PartialEq + std::fmt::Debug,
    I: IntoIterator<Item = T> + Element,
{
    source: Rc<dyn Stream<I>>,
    func: F,
    #[debug(skip)]
    map: DemuxMap<K>,
    children: Rc<RefCell<Vec<Rc<dyn Stream<TinyVec<[T; 1]>>>>>>,
    overflow_child: Rc<RefCell<Option<Rc<dyn Stream<TinyVec<[T; 1]>>>>>>,
    value: Vec<TinyVec<[T; 1]>>,
    #[new(default)]
    // map from child node index to its index in the graph
    index_map: Vec<usize>,
    #[new(default)]
    overflow_graph_index: Option<usize>,
}

impl<T, F, K, I> StreamPeekRef<Vec<TinyVec<[T; 1]>>> for DemuxVecParent<T, F, K, I>
where
    T: Element,
    F: Fn(&T) -> (K, DemuxEvent),
    K: Hash + Eq + PartialEq + std::fmt::Debug,
    I: IntoIterator<Item = T> + Element,
{
    fn peek_ref(&self) -> &Vec<TinyVec<[T; 1]>> {
        &self.value
    }
}

impl<T, K, F, I> MutableNode for DemuxVecParent<T, F, K, I>
where
    T: Element,
    F: Fn(&T) -> (K, DemuxEvent),
    K: Hash + Eq + PartialEq + std::fmt::Debug,
    I: IntoIterator<Item = T> + Element,
{
    fn cycle(&mut self, graph_state: &mut GraphState) -> bool {
        for row in &mut self.value {
            row.clear();
        }
        for item in self.source.peek_value() {
            let (key, event) = (self.func)(&item);
            let entry = match event {
                DemuxEvent::Close => self.map.release(&key),
                DemuxEvent::None => self.map.get_or_insert(key),
            };
            let (index, graph_index) = match entry {
                DemuxEntry::Overflow => {
                    let index = self.map.size();
                    let graph_index = self.overflow_graph_index.unwrap();
                    (index, graph_index)
                }
                DemuxEntry::Some(index) => {
                    let graph_index = self.index_map[index];
                    (index, graph_index)
                }
            };
            self.value[index].push(item);
            // mark dirty directly instead of ticking
            graph_state.mark_dirty(graph_index);
        }
        false
    }

    fn setup(&mut self, graph_state: &mut GraphState) {
        let mut childes = self.children.borrow_mut();
        let mut node_indexes: Vec<_> = childes
            .drain(..)
            .map(|strm| {
                graph_state.node_index(strm.as_node()).unwrap_or_else(|| {
                    panic!("Failed to resolve graph index of demux child node.  Was it added to the graph?")
                })
            })
            .collect();
        drop(childes);
        node_indexes
            .drain(..)
            .for_each(|node_index| self.index_map.push(node_index));
        let overflow = self.overflow_child.take().unwrap();
        self.overflow_graph_index = graph_state.node_index(overflow.as_node());
        assert!(
            self.overflow_graph_index.is_some(),
            "Failed to resolve graph index of demux overflow node.  Was it added to the graph?"
        );
        assert!(
            !self.index_map.is_empty(),
            "Failed to resolve any children to demux into"
        );
    }

    fn upstreams(&self) -> UpStreams {
        let nodes = vec![self.source.clone().as_node()];
        UpStreams::new(nodes, vec![])
    }
}

#[derive(new)]
struct DemuxVecChild<T>
where
    T: Element,
{
    index: usize,
    source: Rc<dyn Stream<Vec<TinyVec<[T; 1]>>>>,
    #[new(default)]
    value: TinyVec<[T; 1]>,
}

impl<T> MutableNode for DemuxVecChild<T>
where
    T: Element,
{
    fn cycle(&mut self, _state: &mut GraphState) -> bool {
        self.value = self.source.peek_ref_cell().get(self.index).unwrap().clone();
        true
    }

    fn upstreams(&self) -> UpStreams {
        // source never ticks but use passive wiring anyway
        UpStreams::new(vec![], vec![self.source.clone().as_node()])
    }
}

impl<T> StreamPeekRef<TinyVec<[T; 1]>> for DemuxVecChild<T>
where
    T: Element,
{
    fn peek_ref(&self) -> &TinyVec<[T; 1]> {
        &self.value
    }
}

#[cfg(test)]
mod tests {

    // export RUST_LOG=INFO; cargo test --lib demux -- --no-capture 2>&1 | grep source | sort | more

    use once_cell::sync::Lazy;
    use std::collections::HashSet;
    use std::fmt::Debug;
    use std::time::Duration;

    use super::DemuxMap;
    use crate::nodes::*;

    type MessageId = (usize, u64);
    type Topic = (usize, u64);

    #[derive(Clone, Default, Debug, PartialEq)]
    enum MessageType {
        #[default]
        Content,
        Close,
    }

    #[derive(Clone, Default, Debug, PartialEq)]
    struct Message {
        #[allow(dead_code)]
        id: MessageId,
        #[allow(dead_code)]
        topic: Topic,
        #[allow(dead_code)]
        message_type: MessageType,
    }

    fn message_source(stream_id: usize, delay: Option<Duration>) -> Rc<dyn Stream<Message>> {
        ticker(*PERIOD)
            .count()
            .delay(delay.unwrap_or(Duration::ZERO))
            .map(move |x| {
                let topic = (stream_id, x / MESSAGES_PER_TOPIC as u64);
                let id = (stream_id, x);
                let message_type = match (x + 1) % MESSAGES_PER_TOPIC as u64 {
                    0 => MessageType::Close,
                    _ => MessageType::Content,
                };
                Message {
                    id,
                    topic,
                    message_type,
                }
            })
            .logged(&format!("source {stream_id} "), log::Level::Info)
    }

    fn parse_message(message: &Message) -> (Topic, DemuxEvent) {
        let key = message.topic;
        let event = match message.message_type {
            MessageType::Close => DemuxEvent::Close,
            MessageType::Content => DemuxEvent::None,
        };
        (key, event)
    }

    fn build_results<T: Element>(
        demuxed: Vec<Rc<dyn Stream<T>>>,
        overflow: Overflow<T>,
        with_overflow: bool,
    ) -> (Vec<Rc<dyn Stream<Vec<T>>>>, Vec<Rc<dyn Node>>) {
        let mut dmxd = demuxed;
        if with_overflow {
            dmxd.push(overflow.stream());
        }
        let results = dmxd
            .iter()
            .enumerate()
            .map(|(i, sream)| {
                sream
                    .logged(&format!("output {i} "), log::Level::Info)
                    .accumulate()
            })
            .collect::<Vec<_>>();
        let mut nodes = results
            .iter()
            .map(|strm| strm.clone().as_node())
            .collect::<Vec<_>>();
        if !with_overflow {
            let overflow = overflow.stream().for_each(|_, _| {
                panic!("overflow!");
            });
            nodes.push(overflow);
        }
        (results, nodes)
    }

    fn validate_results<T>(results: Vec<Rc<dyn Stream<Vec<T>>>>, func: impl Fn(&T) -> Topic) {
        let topics = results
            .iter()
            .map(|strm| {
                strm.peek_value()
                    .iter()
                    .map(|item| func(item))
                    .collect::<HashSet<_>>()
            })
            .collect::<Vec<_>>();
        let sorted_topics = topics
            .clone()
            .iter()
            .map(|item| {
                let mut res = item.clone().into_iter().collect::<Vec<_>>();
                res.sort();
                res
            })
            .collect::<Vec<_>>();
        let n_msgs = results
            .iter()
            .map(|strm| strm.peek_value().len())
            .collect::<Vec<_>>();
        println!("n_msgs = {n_msgs:?}");
        println!("ntopics:");
        sorted_topics.iter().for_each(|item| {
            println!("{:?}", item);
        });
        println!("");
        for rw in sorted_topics {
            assert_eq!(rw.len(), EXPECTED_DISTINCT_TOPICS_PER_STREAM);
        }
        n_msgs
            .into_iter()
            .for_each(|n| assert!(n >= EXPECTED_MIN_MSGS_PER_STREAM));
    }

    fn capacity(with_overflow: bool) -> usize {
        if with_overflow { N_STREAMS - 1 } else { N_STREAMS }
    }

    const N_STREAMS: usize = 3;
    const MESSAGES_PER_TOPIC: u32 = 5;
    const DELAY: Duration = Duration::from_micros(1);
    const PERIOD: Lazy<Duration> = Lazy::new(|| DELAY * MESSAGES_PER_TOPIC * 2);
    const RUN_FOR: Lazy<RunFor> = Lazy::new(|| RunFor::Duration(*PERIOD * MESSAGES_PER_TOPIC * 3));
    const EXPECTED_DISTINCT_TOPICS_PER_STREAM: usize = 4;
    const EXPECTED_MIN_MSGS_PER_STREAM: usize = 15;
    const RUN_MODES: Lazy<Vec<RunMode>> = Lazy::new(|| {
        vec![
            RunMode::HistoricalFrom(NanoTime::ZERO),
            //RunMode::RealTime,
        ]
    });

    fn run_demux_vec(with_overflow: bool) {
        let capacity = capacity(with_overflow);
        for run_mode in &*RUN_MODES {
            println!("demux_vec\n{:?}\nwith_overflow={:?}", run_mode, with_overflow);
            let streams = (0..N_STREAMS)
                .map(|i| message_source(i, None))
                .collect::<Vec<_>>();
            let map = DemuxMap::new(capacity);
            let muxed = combine(streams);
            let (demuxed, overflow) = muxed.demux_it_with_map(map, parse_message);
            let (results, nodes) = build_results(demuxed, overflow, with_overflow);
            Graph::new(nodes, *run_mode, *RUN_FOR).run().unwrap();
            let parse_topic = |msgs: &TinyVec<[Message; 1]>| {
                assert!(msgs.len() == 1);
                msgs[0].topic
            };
            validate_results(results, parse_topic);
        }
    }

    fn run_demux(with_overflow: bool) {
        let capacity = capacity(with_overflow);
        for run_mode in &*RUN_MODES {
            println!("demux\n{:?}\nwith_overflow={:?}", run_mode, with_overflow);
            let streams = (0..N_STREAMS)
                .map(|i| message_source(i, Some(DELAY * i as u32)))
                .collect::<Vec<_>>();
            let muxed = merge(streams);
            let (demuxed, overflow) = muxed.demux(capacity, parse_message);
            let (results, nodes) = build_results(demuxed, overflow, with_overflow);
            Graph::new(nodes, *run_mode, *RUN_FOR).run().unwrap();
            validate_results(results, |msg| msg.topic);
        }
    }

    #[test]
    pub fn demux_works() {
        let _ = env_logger::try_init();
        run_demux(false);
        run_demux(true);
        run_demux_vec(false);
        run_demux_vec(true);
    }
}
