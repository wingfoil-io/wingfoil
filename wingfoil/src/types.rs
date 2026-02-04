use derive_new::new;
use std::cell::RefCell;
use std::fmt::{Debug, Display};
use std::rc::Rc;

pub use crate::graph::GraphState;
pub use crate::time::*;

/// The graph can ask a [Node] what it's upstreams sources are.  The node
/// replies with a [UpStreams] for passive and active sources.   All sources
/// are wired upstream.   Active nodes trigger [Node].cycle() when they tick.
/// Passive [Node]s do not.   
#[derive(new, Default)]
pub struct UpStreams<'a> {
    pub active: Vec<Rc<dyn Node<'a> + 'a>>,
    pub passive: Vec<Rc<dyn Node<'a> + 'a>>,
}

impl<'a> UpStreams<'a> {
    pub fn none() -> UpStreams<'a> {
        UpStreams::new(Vec::new(), Vec::new())
    }
}

/// [Stream]s produce values constrained by this trait. 
pub trait Element: Debug + Clone {}

impl<T> Element for T where T: Debug + Clone {}

/// Implement this trait create your own [Node].
pub trait MutableNode<'a> {
    /// Called by the graph when it determines that this node
    /// is required to be cycled.
    /// Returns Ok(true) if the node's state changed, Ok(false) otherwise.
    fn cycle(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<bool>;
    /// Called by the graph at wiring time.
    fn upstreams(&self) -> UpStreams<'a>;
    /// called by the graph after wiring and before start
    #[allow(unused_variables)]
    fn setup(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        Ok(())
    }
    /// Called by the graph after wiring and before the first cycle.
    /// Can be used to request an initial callback.
    #[allow(unused_variables)]
    fn start(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        Ok(())
    }
    /// Called by the graph after the last cycle.  Can be used to clean up resources.
    #[allow(unused_variables)]
    fn stop(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        Ok(())
    }
    #[allow(unused_variables)]
    fn teardown(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        Ok(())
    }

    fn type_name(&self) -> String {
        tynm::type_name::<Self>()
    }
}

impl<'a> Display for dyn Node<'a> + 'a {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.type_name())
    }
}

impl<'a, T> Debug for dyn Stream<'a, T> + 'a {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.type_name())
    }
}

/// A wiring point in the Graph.
pub trait Node<'a>: MutableNode<'a> {
    /// This is like Node::cycle but doesn't require mutable self
    /// Returns Ok(true) if the node's state changed, Ok(false) otherwise.
    fn cycle(&self, state: &mut GraphState<'a>) -> anyhow::Result<bool>;
    fn setup(&self, state: &mut GraphState<'a>) -> anyhow::Result<()>;
    fn start(&self, state: &mut GraphState<'a>) -> anyhow::Result<()>;
    fn stop(&self, state: &mut GraphState<'a>) -> anyhow::Result<()>;
    fn teardown(&self, state: &mut GraphState<'a>) -> anyhow::Result<()>;
}

/// A trait through which a reference to [Stream]'s value can
/// be peeked at.
pub trait StreamPeekRef<'a, T>: MutableNode<'a> {
    fn peek_ref(&self) -> &T;
    fn clone_from_cell_ref(&self, cell_ref: std::cell::Ref<'_, T>) -> T where T: Clone {
        cell_ref.clone()
    }
}

/// The trait through which a [Stream]s can current value
/// can be peeked at.
pub trait StreamPeek<'a, T> {
    fn peek_value(&self) -> T where T: Clone;
    fn peek_ref_cell(&self) -> std::cell::Ref<'_, T>;
}

/// A [Node] which has some state that can peeked at.
pub trait Stream<'a, T>: Node<'a> + StreamPeek<'a, T> + AsNode<'a> {}

// RefCell

impl<'a, NODE: MutableNode<'a>> Node<'a> for RefCell<NODE> {
    fn cycle(&self, state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.borrow_mut().cycle(state)
    }
    fn setup(&self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        self.borrow_mut().setup(state)
    }
    fn start(&self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        self.borrow_mut().start(state)
    }
    fn stop(&self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        self.borrow_mut().stop(state)
    }
    fn teardown(&self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        self.borrow_mut().teardown(state)
    }
}

impl<'a, NODE: MutableNode<'a>> MutableNode<'a> for RefCell<NODE> {
    fn cycle(&mut self, graph_state: &mut GraphState<'a>) -> anyhow::Result<bool> {
        self.borrow_mut().cycle(graph_state)
    }
    fn upstreams(&self) -> UpStreams<'a> {
        self.borrow().upstreams()
    }
    fn start(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        self.borrow_mut().start(state)
    }
    fn stop(&mut self, state: &mut GraphState<'a>) -> anyhow::Result<()> {
        self.borrow_mut().stop(state)
    }
    fn type_name(&self) -> String {
        self.borrow().type_name()
    }
}

impl<'a, STREAM, T> StreamPeek<'a, T> for RefCell<STREAM>
where
    STREAM: StreamPeekRef<'a, T>,
{
    fn peek_ref_cell(&self) -> std::cell::Ref<'_, T> {
        std::cell::Ref::map(self.borrow(), |strm| strm.peek_ref())
    }
    fn peek_value(&self) -> T where T: Clone {
        self.borrow().clone_from_cell_ref(self.peek_ref_cell())
    }
}

impl<'a, STREAM, T> Stream<'a, T> for RefCell<STREAM>
where
    STREAM: StreamPeekRef<'a, T> + 'a,
{
}

/// Used to cast Rc<dyn [Stream]> to Rc<dyn [Node]>
pub trait AsNode<'a> {
    fn as_node(self: Rc<Self>) -> Rc<dyn Node<'a> + 'a>;
}

impl<'a, NODE: Node<'a> + 'a> AsNode<'a> for NODE {
    fn as_node(self: Rc<Self>) -> Rc<dyn Node<'a> + 'a> {
        self
    }
}

/// Used co cast Rc of concrete stream into Rc of dyn [Stream].
pub trait AsStream<'a, T> {
    fn as_stream(self: Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a>;
}

impl<'a, T, STREAM: Stream<'a, T> + 'a> AsStream<'a, T> for STREAM {
    fn as_stream(self: Rc<Self>) -> Rc<dyn Stream<'a, T> + 'a> {
        self
    }
}

/// Used to consume a concrete [MutableNode] and return
/// an Rc<dyn [Node]>>.
pub trait IntoNode<'a> {
    fn into_node(self) -> Rc<dyn Node<'a> + 'a>;
}

impl<'a, NODE: MutableNode<'a> + 'a> IntoNode<'a> for NODE {
    fn into_node(self) -> Rc<dyn Node<'a> + 'a> {
        Rc::new(RefCell::new(self))
    }
}

/// Used to consume a concrete [Stream] and return
/// an Rc<dyn [Stream]>>.
pub trait IntoStream<'a, T> {
    fn into_stream(self) -> Rc<dyn Stream<'a, T> + 'a>;
}

impl<'a, T, STREAM> IntoStream<'a, T> for STREAM
where
    STREAM: StreamPeekRef<'a, T> + 'a,
{
    fn into_stream(self) -> Rc<dyn Stream<'a, T> + 'a> {
        Rc::new(RefCell::new(self))
    }
}
