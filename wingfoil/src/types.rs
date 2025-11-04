use derive_new::new;
use std::cell::RefCell;
use std::fmt::{Debug, Display};
use std::rc::Rc;

pub use crate::graph::GraphState;
pub use crate::time::*;

/// The graph can ask a [Node] what it's upstreams sources are.  The node
/// replies wiht a [UpStreams] for passive and active sources.   All sources
/// are wired upstream.   Active nodes trigger [Node].cycle() when they tick.
/// Passive [Node]s do not.   
#[derive(new, Default)]
pub struct UpStreams {
    pub active: Vec<Rc<dyn Node>>,
    pub passive: Vec<Rc<dyn Node>>,
}

impl UpStreams {
    pub fn none() -> UpStreams {
        UpStreams::new(Vec::new(), Vec::new())
    }
}

/// [Stream]s produce values constrained by this trait.  For large structs that you
/// would prefer not to clone, it is recomended to wrap them in a [Rc](std::rc::Rc)
/// so they can be cloned cheaply.
#[doc(hidden)]
pub trait Element: Debug + Clone + Default + 'static {}

impl<T> Element for T where T: Debug + Clone + Default + 'static {}

/// Implement this trait create your own [Node].
pub trait MutableNode {
    /// Called by the graph when it determines that this node
    /// is required to be cycled.
    fn cycle(&mut self, state: &mut GraphState) -> bool;
    /// Called by the graph at wiring time.
    fn upstreams(&self) -> UpStreams {
        UpStreams::default()
    }
    /// called by the graph after wiring and before start
    #[allow(unused_variables)]
    fn setup(&mut self, state: &mut GraphState) {}
    /// Called by the graph after wiring and before the first cycle.
    /// Can be used to request an initial callback.
    #[allow(unused_variables)]
    fn start(&mut self, state: &mut GraphState) {}
    /// Called by the graph after the last cycle.  Can be used to clean up resources.
    #[allow(unused_variables)]
    fn stop(&mut self, state: &mut GraphState) {}
    #[allow(unused_variables)]
    fn teardown(&mut self, state: &mut GraphState) {}

    fn type_name(&self) -> String {
        tynm::type_name::<Self>()
    }
}

impl Display for dyn Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.type_name())
    }
}

impl<T> Debug for dyn Stream<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.type_name())
    }
}

/// A wiring point in the Graph.
pub trait Node: MutableNode {
    /// This is like Node::cycle but doesn't require mutable self
    fn cycle(&self, state: &mut GraphState) -> bool;
    fn setup(&self, state: &mut GraphState);
    fn start(&self, state: &mut GraphState);
    fn stop(&self, state: &mut GraphState);
    fn teardown(&self, state: &mut GraphState);
}

/// A trait through which a referene to [Stream]'s value can
/// be peeked at.
pub trait StreamPeekRef<T>: MutableNode {
    fn peek_ref(&self) -> &T;
}

/// The trait through which a [Stream]s can current value
/// can be peeked at.
pub trait StreamPeek<T> {
    fn peek_value(&self) -> T;
    fn peek_ref_cell(&self) -> std::cell::Ref<'_, T>;
}

/// A [Node] which has some state that can peeked at.
pub trait Stream<T>: Node + StreamPeek<T> + AsNode {}

// RefCell

impl<NODE: MutableNode> Node for RefCell<NODE> {
    fn cycle(&self, state: &mut GraphState) -> bool {
        self.borrow_mut().cycle(state)
    }
    fn setup(&self, state: &mut GraphState) {
        self.borrow_mut().setup(state)
    }
    fn start(&self, state: &mut GraphState) {
        self.borrow_mut().start(state)
    }
    fn stop(&self, state: &mut GraphState) {
        self.borrow_mut().stop(state)
    }
    fn teardown(&self, state: &mut GraphState) {
        self.borrow_mut().teardown(state)
    }
}

impl<NODE: MutableNode> MutableNode for RefCell<NODE> {
    fn cycle(&mut self, graph_state: &mut GraphState) -> bool {
        self.borrow_mut().cycle(graph_state)
    }
    fn upstreams(&self) -> UpStreams {
        self.borrow().upstreams()
    }
    fn start(&mut self, state: &mut GraphState) {
        self.borrow_mut().start(state)
    }
    fn stop(&mut self, state: &mut GraphState) {
        self.borrow_mut().stop(state)
    }
}

impl<STREAM, T> StreamPeek<T> for RefCell<STREAM>
where
    STREAM: StreamPeekRef<T>,
    T: Clone,
{
    fn peek_ref_cell(&self) -> std::cell::Ref<'_, T> {
        std::cell::Ref::map(self.borrow(), |strm| strm.peek_ref())
    }
    fn peek_value(&self) -> T {
        self.peek_ref_cell().clone()
    }
}

impl<STREAM, T> Stream<T> for RefCell<STREAM>
where
    STREAM: StreamPeekRef<T> + 'static,
    T: Clone + 'static,
{
}

/// Used to cast Rc<dyn [Stream]> to Rc<dyn [Node]>
pub trait AsNode {
    fn as_node(self: Rc<Self>) -> Rc<dyn Node>;
}

impl<NODE: Node + 'static> AsNode for NODE {
    fn as_node(self: Rc<Self>) -> Rc<dyn Node> {
        self
    }
}

/// Used co cast Rc of concrete stream into Rc of dyn [Stream].
pub trait AsStream<T> {
    fn as_stream(self: Rc<Self>) -> Rc<dyn Stream<T>>;
}

impl<T, STREAM: Stream<T> + 'static> AsStream<T> for STREAM {
    fn as_stream(self: Rc<Self>) -> Rc<dyn Stream<T>> {
        self
    }
}

/// Used to consume a concrete [MutableNode] and return
/// an Rc<dyn [Node]>>.
pub trait IntoNode {
    fn into_node(self) -> Rc<dyn Node>;
}

impl<NODE: MutableNode + 'static> IntoNode for NODE {
    fn into_node(self) -> Rc<dyn Node> {
        Rc::new(RefCell::new(self))
    }
}

/// Used to consume a concrete [Stream] and return
/// an Rc<dyn [Stream]>>.
pub trait IntoStream<T> {
    fn into_stream(self) -> Rc<dyn Stream<T>>;
}

impl<T, STREAM> IntoStream<T> for STREAM
where
    T: Clone + 'static,
    STREAM: StreamPeekRef<T> + 'static,
{
    fn into_stream(self) -> Rc<dyn Stream<T>> {
        Rc::new(RefCell::new(self))
    }
}
