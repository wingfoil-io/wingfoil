//! [`DynamicGroup`]: a keyed collection of dynamically-wired streams.

use std::collections::{BTreeMap, HashMap};
use std::hash::Hash;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::graph::GraphState;
use crate::types::{
    Element, IntoStream, MutableNode, Stream, StreamPeekRef, UpStreams, WiringPoint,
};

/// Backing-store abstraction for [`DynamicGroup`].
///
/// Blanket implementations are provided for [`BTreeMap`] and [`HashMap`].
/// Implement this trait to use a custom collection.
pub trait StreamStore<K, T: Element> {
    fn store_insert(&mut self, key: K, stream: Rc<dyn Stream<T>>);
    fn store_remove(&mut self, key: &K) -> Option<Rc<dyn Stream<T>>>;
    fn store_entries<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a Rc<dyn Stream<T>>)>
    where
        K: 'a;
}

impl<K: Ord, T: Element> StreamStore<K, T> for BTreeMap<K, Rc<dyn Stream<T>>> {
    fn store_insert(&mut self, key: K, stream: Rc<dyn Stream<T>>) {
        self.insert(key, stream);
    }

    fn store_remove(&mut self, key: &K) -> Option<Rc<dyn Stream<T>>> {
        self.remove(key)
    }

    fn store_entries<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a Rc<dyn Stream<T>>)>
    where
        K: 'a,
    {
        self.iter()
    }
}

impl<K: Hash + Eq, T: Element> StreamStore<K, T> for HashMap<K, Rc<dyn Stream<T>>> {
    fn store_insert(&mut self, key: K, stream: Rc<dyn Stream<T>>) {
        self.insert(key, stream);
    }

    fn store_remove(&mut self, key: &K) -> Option<Rc<dyn Stream<T>>> {
        self.remove(key)
    }

    fn store_entries<'a>(&'a self) -> impl Iterator<Item = (&'a K, &'a Rc<dyn Stream<T>>)>
    where
        K: 'a,
    {
        self.iter()
    }
}

/// A keyed collection of dynamically-wired streams.
///
/// Keeps your stream registry and graph wiring in sync:
/// [`insert`](DynamicGroup::insert) calls `state.add_upstream` for you, and
/// [`remove`](DynamicGroup::remove) calls `state.remove_node`.  Use
/// [`ticked_iter`](DynamicGroup::ticked_iter) in your `cycle()` to visit only
/// the streams that fired this engine tick.
///
/// The backing store `S` defaults to [`BTreeMap`] (requires `K: Ord`).
/// Pass a [`HashMap`] or any [`StreamStore`] implementation to
/// [`with_store`](DynamicGroup::with_store) to use a different collection.
///
/// # Example
/// ```ignore
/// // BTreeMap-backed (default)
/// let mut group: DynamicGroup<String, Price> = DynamicGroup::new();
///
/// // HashMap-backed
/// let mut group = DynamicGroup::with_store(HashMap::<String, _>::new());
/// ```
pub struct DynamicGroup<K, T: Element, S: StreamStore<K, T> = BTreeMap<K, Rc<dyn Stream<T>>>> {
    store: S,
    _phantom: PhantomData<(K, T)>,
}

impl<K: Ord, T: Element> DynamicGroup<K, T> {
    /// Creates a new `DynamicGroup` backed by a [`BTreeMap`].
    pub fn new() -> Self {
        Self::with_store(BTreeMap::new())
    }
}

impl<K: Ord, T: Element> Default for DynamicGroup<K, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, T: Element, S: StreamStore<K, T>> DynamicGroup<K, T, S> {
    /// Creates a `DynamicGroup` backed by a custom [`StreamStore`].
    pub fn with_store(store: S) -> Self {
        Self {
            store,
            _phantom: PhantomData,
        }
    }

    /// Register `stream` under `key` and wire it into the graph as an active upstream.
    ///
    /// Equivalent to calling `state.add_upstream(stream, true, true)` and inserting
    /// into the backing store.  The recycle fires the attachment points of the new
    /// subgraph (nodes that connect directly to the pre-existing graph) at `t+1ns`,
    /// so filter-based subgraphs correctly evaluate the shared source's current value
    /// rather than reading stale defaults from intermediate nodes.
    pub fn insert(&mut self, state: &mut GraphState, key: K, stream: Rc<dyn Stream<T>>) {
        state.add_upstream(stream.clone().as_node(), true, true);
        self.store.store_insert(key, stream);
    }

    /// Remove the stream registered under `key` and unwire it from the graph.
    ///
    /// Does nothing if `key` is not present.
    pub fn remove(&mut self, state: &mut GraphState, key: &K) {
        if let Some(stream) = self.store.store_remove(key) {
            state.remove_node(stream.as_node());
        }
    }

    /// Iterate `(key, value)` pairs for streams that ticked this engine cycle.
    pub fn ticked_iter<'a>(&'a self, state: &'a GraphState) -> impl Iterator<Item = (&'a K, T)> + 'a
    where
        K: 'a,
    {
        self.store
            .store_entries()
            .filter(|&(_, s)| state.ticked(s.clone().as_node()))
            .map(|(k, s)| (k, s.peek_value()))
    }
}

// ── DynamicGroupStream ────────────────────────────────────────────────────────

struct DynamicGroupStream<K: Element, T: Element, V: Element, S: StreamStore<K, T>> {
    add: Rc<dyn Stream<K>>,
    del: Rc<dyn Stream<K>>,
    factory: Box<dyn Fn(K) -> Rc<dyn Stream<T>>>,
    group: DynamicGroup<K, T, S>,
    on_tick: Box<dyn Fn(&mut V, &K, T)>,
    on_remove: Box<dyn Fn(&mut V, &K)>,
    value: V,
}

impl<K, T, V, S> WiringPoint for DynamicGroupStream<K, T, V, S>
where
    K: Element,
    T: Element,
    V: Element,
    S: StreamStore<K, T>,
{
    fn upstreams(&self) -> UpStreams {
        UpStreams::new(
            vec![self.add.clone().as_node(), self.del.clone().as_node()],
            vec![],
        )
    }
}

impl<K, T, V, S> MutableNode for DynamicGroupStream<K, T, V, S>
where
    K: Element,
    T: Element,
    V: Element,
    S: StreamStore<K, T>,
{
    fn cycle(&mut self, state: &mut GraphState) -> anyhow::Result<bool> {
        if state.ticked(self.add.clone().as_node()) {
            let key = self.add.peek_value();
            let stream = (self.factory)(key.clone());
            self.group.insert(state, key, stream);
        }
        if state.ticked(self.del.clone().as_node()) {
            let key = self.del.peek_value();
            // on_remove fires before graph unwiring so the user can clean up
            // output state while the stream is still addressable by key.
            (self.on_remove)(&mut self.value, &key);
            self.group.remove(state, &key);
        }
        let mut ticked = false;
        for (key, val) in self.group.ticked_iter(state) {
            (self.on_tick)(&mut self.value, key, val);
            ticked = true;
        }
        Ok(ticked)
    }
}

impl<K, T, V, S> StreamPeekRef<V> for DynamicGroupStream<K, T, V, S>
where
    K: Element,
    T: Element,
    V: Element,
    S: StreamStore<K, T>,
{
    fn peek_ref(&self) -> &V {
        &self.value
    }
}

/// Creates a [`Stream`] that maintains a dynamic group of per-key subgraphs.
///
/// - `add` — when it ticks, `factory` is called with its value to create a new per-key stream
/// - `del` — when it ticks, the stream for that key is removed from the group
/// - `factory` — builds the per-key subgraph given a key
/// - `init` — initial value of the output stream
/// - `on_tick` — called for each per-key stream that ticked; update the output value
/// - `on_remove` — called when a key is deleted; clean up any state in the output value
///
/// The backing store defaults to [`BTreeMap`] (requires `K: Ord`). Use
/// [`dynamic_group_stream_with_store`] for a custom [`StreamStore`].
pub fn dynamic_group_stream<K, T, V>(
    add: Rc<dyn Stream<K>>,
    del: Rc<dyn Stream<K>>,
    factory: impl Fn(K) -> Rc<dyn Stream<T>> + 'static,
    init: V,
    on_tick: impl Fn(&mut V, &K, T) + 'static,
    on_remove: impl Fn(&mut V, &K) + 'static,
) -> Rc<dyn Stream<V>>
where
    K: Element + Ord,
    T: Element,
    V: Element,
{
    dynamic_group_stream_with_store(add, del, factory, BTreeMap::new(), init, on_tick, on_remove)
}

/// Like [`dynamic_group_stream`] but with a custom [`StreamStore`] backing the group.
pub fn dynamic_group_stream_with_store<K, T, V, S>(
    add: Rc<dyn Stream<K>>,
    del: Rc<dyn Stream<K>>,
    factory: impl Fn(K) -> Rc<dyn Stream<T>> + 'static,
    store: S,
    init: V,
    on_tick: impl Fn(&mut V, &K, T) + 'static,
    on_remove: impl Fn(&mut V, &K) + 'static,
) -> Rc<dyn Stream<V>>
where
    K: Element,
    T: Element,
    V: Element,
    S: StreamStore<K, T> + 'static,
{
    DynamicGroupStream {
        add,
        del,
        factory: Box::new(factory),
        group: DynamicGroup::with_store(store),
        on_tick: Box::new(on_tick),
        on_remove: Box::new(on_remove),
        value: init,
    }
    .into_stream()
}
