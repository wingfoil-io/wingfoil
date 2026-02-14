use derive_new::new;
use std::cell::RefCell;
use std::fmt::{Debug, Display};
use std::rc::Rc;
use tinyvec::TinyVec;

pub use crate::graph::GraphState;
pub use crate::time::*;

/// A small vector optimised for single-element bursts.
///
/// In multi-threaded or async contexts, multiple values may arrive between
/// engine cycles, so incoming data is always a `Burst<T>` rather than a
/// plain `T`.  Use [`.collapse()`](crate::StreamOperators::collapse) to
/// reduce a burst to its latest value.
pub type Burst<T> = TinyVec<[T; 1]>;

/// Wraps a [Stream] to indicate whether it is an active or passive dependency.
/// Active dependencies trigger downstream nodes when they tick.
/// Passive dependencies are read but don't trigger execution.
pub enum Dep<T> {
    Active(Rc<dyn Stream<T>>),
    Passive(Rc<dyn Stream<T>>),
}

impl<T> Dep<T> {
    pub fn stream(&self) -> &Rc<dyn Stream<T>> {
        match self {
            Dep::Active(s) | Dep::Passive(s) => s,
        }
    }

    pub fn is_active(&self) -> bool {
        matches!(self, Dep::Active(_))
    }

    #[must_use]
    pub fn as_node(&self) -> Rc<dyn Node>
    where
        T: 'static,
    {
        self.stream().clone().as_node()
    }
}

/// The graph can ask a [Node] what it's upstreams sources are.  The node
/// replies with a [UpStreams] for passive and active sources.   All sources
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
/// would prefer not to clone, it is recommended to wrap them in a [Rc](std::rc::Rc)
/// so they can be cloned cheaply.
#[doc(hidden)]
pub trait Element: Debug + Clone + Default + 'static {}

impl<T> Element for T where T: Debug + Clone + Default + 'static {}

/// Implement this trait create your own [Node].
pub trait MutableNode {
    /// Called by the graph when it determines that this node
    /// is required to be cycled.
    /// Returns Ok(true) if the node's state changed, Ok(false) otherwise.
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool>;
    /// Called by the graph at wiring time.
    fn upstreams(&self) -> UpStreams;
    /// called by the graph after wiring and before start
    #[allow(unused_variables)]
    fn setup(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        Ok(())
    }
    /// Called by the graph after wiring and before the first cycle.
    /// Can be used to request an initial callback.
    #[allow(unused_variables)]
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        Ok(())
    }
    /// Called by the graph after the last cycle.  Can be used to clean up resources.
    #[allow(unused_variables)]
    fn stop(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        Ok(())
    }
    #[allow(unused_variables)]
    fn teardown(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        Ok(())
    }

    fn type_name(&self) -> String {
        tynm::type_name::<Self>()
    }
}

impl Display for dyn Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name())
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
    /// Returns Ok(true) if the node's state changed, Ok(false) otherwise.
    fn cycle(&self, state: &mut GraphState) -> anyhow::Result<bool>;
    fn setup(&self, state: &mut GraphState) -> anyhow::Result<()>;
    fn start(&self, state: &mut GraphState) -> anyhow::Result<()>;
    fn stop(&self, state: &mut GraphState) -> anyhow::Result<()>;
    fn teardown(&self, state: &mut GraphState) -> anyhow::Result<()>;
}

/// A trait through which a reference to [Stream]'s value can
/// be peeked at.
pub trait StreamPeekRef<T: Clone>: MutableNode {
    fn peek_ref(&self) -> &T;
    fn clone_from_cell_ref(&self, cell_ref: std::cell::Ref<'_, T>) -> T {
        cell_ref.clone()
    }
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
    fn cycle(&self, state: &mut GraphState) -> anyhow::Result<bool> {
        self.borrow_mut().cycle(state)
    }
    fn setup(&self, state: &mut GraphState) -> anyhow::Result<()> {
        self.borrow_mut().setup(state)
    }
    fn start(&self, state: &mut GraphState) -> anyhow::Result<()> {
        self.borrow_mut().start(state)
    }
    fn stop(&self, state: &mut GraphState) -> anyhow::Result<()> {
        self.borrow_mut().stop(state)
    }
    fn teardown(&self, state: &mut GraphState) -> anyhow::Result<()> {
        self.borrow_mut().teardown(state)
    }
}

impl<NODE: MutableNode> MutableNode for RefCell<NODE> {
    fn cycle(&mut self, graph_state: &mut GraphState) -> anyhow::Result<bool> {
        self.borrow_mut().cycle(graph_state)
    }
    fn upstreams(&self) -> UpStreams {
        self.borrow().upstreams()
    }
    fn start(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.borrow_mut().start(state)
    }
    fn stop(&mut self, state: &mut GraphState) -> anyhow::Result<()> {
        self.borrow_mut().stop(state)
    }
    fn type_name(&self) -> String {
        self.borrow().type_name()
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
        self.borrow().clone_from_cell_ref(self.peek_ref_cell())
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
    #[must_use]
    fn as_node(self: Rc<Self>) -> Rc<dyn Node>;
}

impl<NODE: Node + 'static> AsNode for NODE {
    fn as_node(self: Rc<Self>) -> Rc<dyn Node> {
        self
    }
}

/// Used co cast Rc of concrete stream into Rc of dyn [Stream].
pub trait AsStream<T> {
    #[must_use]
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
    #[must_use]
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
    #[must_use]
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
