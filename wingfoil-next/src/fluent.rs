//! Fluent wiring sugar: the classic wingfoil chaining style
//! (`ticker(d).count().map(f).filter(&cond)`) over the explicit
//! [`Builder`](crate::interp::Builder) core.
//!
//! A [`Stream<T>`] is a typed handle that also carries a shared reference to
//! the graph under construction, so combinators can be methods. This layer
//! is *wiring-time only* — it adds nothing to execution (the built
//! [`Runner`] is identical), and a future `graph!` macro or recording engine
//! sits at exactly this surface.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use anyhow::Result;

use crate::interp::{AsHandle, Builder, ExternalSource, Handle, Runner};

/// A graph under construction. Cheap to clone; all clones share the same
/// underlying builder.
#[derive(Clone, Default)]
pub struct GraphBuilder {
    inner: Rc<RefCell<Builder>>,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// A source that ticks at a fixed interval.
    pub fn ticker(&self, period: Duration) -> Stream<()> {
        let handle = self.inner.borrow_mut().ticker(period);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }

    /// A source that ticks once with `value` on the first cycle.
    pub fn constant<T: Clone + Default + 'static>(&self, value: T) -> Stream<T> {
        let handle = self.inner.borrow_mut().constant(value);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }

    /// An external source: values sent through the returned
    /// [`ExternalSource`] (from any thread or async task) tick the stream.
    /// If several values arrive between cycles the latest wins. Graphs with
    /// external sources run in realtime mode only.
    pub fn external<T: Clone + Default + 'static>(&self) -> (Stream<T>, ExternalSource<T>) {
        let (handle, source) = self.inner.borrow_mut().external();
        (
            Stream {
                inner: self.inner.clone(),
                handle,
            },
            source,
        )
    }

    /// Mount a composite node (see [`Builder::composite`]). Used by the
    /// `graph!` macro's `nested` expansion; not intended to be called by
    /// hand.
    #[doc(hidden)]
    pub fn __composite<T: Clone + Default + 'static>(
        &self,
        active_ups: Vec<usize>,
        callback_activated: bool,
        node: impl FnMut(&mut crate::op::Ctx, bool) -> Result<crate::op::Tick<T>> + 'static,
    ) -> Stream<T> {
        let handle = self
            .inner
            .borrow_mut()
            .composite(active_ups, callback_activated, node);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }

    /// The shared per-cycle tick flags. Used by the `graph!` macro's
    /// `nested` expansion.
    #[doc(hidden)]
    pub fn __ticked(&self) -> Rc<RefCell<Vec<bool>>> {
        self.inner.borrow().ticked_rc()
    }

    /// A busy-poll source: `f` runs once per engine cycle, ticking on
    /// `Some`. Lossless and ordered — one value per cycle, no coalescing
    /// (contrast [`external`](Self::external), which collapses to
    /// latest-wins). The graph becomes a busy-spin loop: the kernel never
    /// parks. Realtime runs only.
    pub fn poll<T, F>(&self, f: F) -> Stream<T>
    where
        T: Clone + Default + 'static,
        F: Fn() -> Option<T> + 'static,
    {
        let handle = self.inner.borrow_mut().poll(f);
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }

    /// Consume the wired graph into a [`Runner`]. Streams stay usable as
    /// value handles (`runner.value(&stream)`); wiring further nodes from
    /// them afterwards is a logic error — they would target an empty builder.
    pub fn build(&self) -> Runner {
        std::mem::take(&mut *self.inner.borrow_mut()).build()
    }
}

/// A typed stream in a graph under construction. Combinators are methods, so
/// wiring chains: `g.ticker(p).count().map(|i| i * 2)`.
pub struct Stream<T> {
    inner: Rc<RefCell<Builder>>,
    handle: Handle<T>,
}

impl<T> Clone for Stream<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            handle: self.handle,
        }
    }
}

impl<T> AsHandle<T> for Stream<T> {
    fn as_handle(&self) -> Handle<T> {
        self.handle
    }
}

impl<T> AsHandle<T> for &Stream<T> {
    fn as_handle(&self) -> Handle<T> {
        self.handle
    }
}

impl<T> Stream<T> {
    /// The underlying engine handle (for [`Runner::value`]).
    pub fn handle(&self) -> Handle<T> {
        self.handle
    }

    /// The shared value slot backing this stream. Used by the `graph!`
    /// macro's `nested` expansion to read composite inputs.
    #[doc(hidden)]
    pub fn __slot(&self) -> Rc<RefCell<T>>
    where
        T: 'static,
    {
        self.inner.borrow().slot(self.handle)
    }

    fn lift<B>(&self, handle: Handle<B>) -> Stream<B> {
        Stream {
            inner: self.inner.clone(),
            handle,
        }
    }
}

impl<T: 'static> Stream<T> {
    /// Apply a closure to each value.
    pub fn map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> B + 'static,
    {
        let h = self.inner.borrow_mut().map(self.handle, f);
        self.lift(h)
    }

    /// Apply a fallible closure to each value; a returned `Err` aborts the
    /// run with context.
    pub fn try_map<B, F>(&self, f: F) -> Stream<B>
    where
        B: Clone + Default + 'static,
        F: Fn(&T) -> Result<B> + 'static,
    {
        let h = self.inner.borrow_mut().try_map(self.handle, f);
        self.lift(h)
    }

    /// Fold values into an accumulator, emitting it after each fold.
    pub fn fold<B, F>(&self, init: B, f: F) -> Stream<B>
    where
        B: Clone + 'static,
        F: Fn(&mut B, &T) + 'static,
    {
        let h = self.inner.borrow_mut().fold(self.handle, init, f);
        self.lift(h)
    }

    /// Combine with another stream; ticks when either input ticks.
    pub fn join<B, C, F>(&self, other: &Stream<B>, f: F) -> Stream<C>
    where
        B: 'static,
        C: Clone + Default + 'static,
        F: Fn(&T, &B) -> C + 'static,
    {
        let h = self.inner.borrow_mut().join(self.handle, other.handle, f);
        self.lift(h)
    }
}

impl<T: Clone + Default + 'static> Stream<T> {
    /// Emit only when `condition`'s current value is true.
    pub fn filter(&self, condition: &Stream<bool>) -> Stream<T> {
        let h = self
            .inner
            .borrow_mut()
            .filter(self.handle, condition.handle);
        self.lift(h)
    }

    /// Emit the current value whenever `trigger` ticks (passive read).
    pub fn sample(&self, trigger: &Stream<()>) -> Stream<T> {
        let h = self.inner.borrow_mut().sample(self.handle, trigger.handle);
        self.lift(h)
    }

    /// Merge with another stream; the earliest-supplied ticked input wins.
    pub fn merge(&self, other: &Stream<T>) -> Stream<T> {
        let h = self.inner.borrow_mut().merge2(self.handle, other.handle);
        self.lift(h)
    }

    /// Collect every emitted value into a `Vec`.
    pub fn accumulate(&self) -> Stream<Vec<T>> {
        self.fold(Vec::new(), |acc, v: &T| acc.push(v.clone()))
    }
}

impl<T: 'static> Stream<T> {
    /// Run a side-effecting (fallible) closure on each tick — the graph's
    /// outbound edge (print, send, record). A returned `Err` aborts the run
    /// with context. Emits `()` per tick.
    pub fn for_each<F>(&self, f: F) -> Stream<()>
    where
        F: Fn(&T) -> Result<()> + 'static,
    {
        let h = self.inner.borrow_mut().for_each(self.handle, f);
        self.lift(h)
    }
}

impl<T: Clone + Default + 'static> Stream<T> {
    /// Run `f` once at teardown — after the run ends, even if a cycle aborted
    /// it. Observes this stream's last value; emits nothing.
    pub fn finally<F>(&self, f: F) -> Stream<()>
    where
        F: Fn(&T) -> Result<()> + 'static,
    {
        let h = self.inner.borrow_mut().finally(self.handle, f);
        self.lift(h)
    }
}

impl<T: Clone + Default + PartialEq + 'static> Stream<T> {
    /// Re-emit each value `delay` later.
    pub fn delay(&self, delay: Duration) -> Stream<T> {
        let h = self.inner.borrow_mut().delay(self.handle, delay);
        self.lift(h)
    }
}

impl Stream<()> {
    /// Running count of ticks: 1, 2, 3, ...
    pub fn count(&self) -> Stream<u64> {
        self.fold(0u64, |acc, _| *acc += 1)
    }
}
