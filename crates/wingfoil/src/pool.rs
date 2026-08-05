//! Pooled payloads: recycled buffers behind cheap graph-side handles, and the
//! loan-based channel producer API ([`pooled_channel`]).
//!
//! [`pooled_channel`]: crate::fluent::SourceOps::pooled_channel
//!
//! # Why this exists
//!
//! The graph already passes values between nodes **by reference** in both
//! tiers (slot borrows interpreted, locals compiled), so transporting a large
//! struct through a chain of transforms was never expensive. The costs that
//! remain sit at the edges and at the ownership points:
//!
//! - **Per-message construction and drop.** A producer that builds a fresh
//!   heap-owning value (a `Vec`-backed book, a decoded message) per send
//!   allocates per send — and the matching frees run on the *graph* thread,
//!   the allocator's worst case (cross-thread arena traffic).
//! - **Routing ops clone to re-emit.** `filter`, `merge`, `sample`, `delay`
//!   and friends receive `&T` and must produce an owned `T` for their own
//!   value slot — one deep clone per routing node per tick.
//!
//! Wrapping payloads in `Arc<T>` fixes only the second (and pays an atomic
//! refcount on every graph-side clone for the sake of a single thread
//! crossing). The pool fixes both:
//!
//! - The producer **loans** a recycled buffer, writes in place — nested `Vec`
//!   capacities survive recycling (`clear()` keeps the heap block), so a
//!   warmed pool serves decode after decode with **zero allocator calls** —
//!   and sends the **unique owner** through the channel: no refcount exists
//!   in flight, nothing is atomic.
//! - The graph-side receiver wraps each arrival in a [`Pooled<T>`] handle —
//!   a **non-atomic** (`Rc`-based) shared reference. Routing clones and
//!   fan-out become refcount bumps. When the last handle drops, the buffer
//!   rides a return queue back to the producer's pool: the one cross-thread
//!   hand-off in the whole lifetime, and no `free()` anywhere.
//! - **A bounded pool is the backpressure.** [`PooledSender::loan`] blocks
//!   (and [`try_loan`](PooledSender::try_loan) fails) once `capacity`
//!   buffers are in flight — the same loan-budget semantics as a
//!   shared-memory transport's segment, surfaced at the same place in the
//!   API.
//!
//! This mirrors the iceoryx2 publisher protocol — *loan, write, send, with a
//! budget* — so in-process and shared-memory ingress share one shape.
//!
//! # Working with handles
//!
//! [`Pooled<T>`] derefs to `T`, so downstream code reads fields as if it held
//! `&T`. The payload behind a handle is **shared and immutable**; an op that
//! wants to add information *wraps* the handle rather than cloning the
//! payload:
//!
//! ```ignore
//! struct Enriched { book: Pooled<Book>, recv: NanoTime }
//! let enriched = books.map_ctx(|b, ctx| Enriched { book: b.clone(), recv: ctx.wall_time() });
//! ```
//!
//! `Clone` on the handle never touches `T` (no `T: Clone` bound anywhere in
//! this module); `PartialEq` is **pointer identity** — two handles are equal
//! iff they refer to the same loan — which is the semantics `delay` /
//! `feedback` dedup wants, and avoids deep comparisons of large payloads.
//!
//! Because value slots are seeded before the first tick, `Pooled<T>` has a
//! [`Default`]: the **empty** handle. A passive read before the stream's
//! first tick (e.g. `sample`) observes it; [`get`](Pooled::get) returns
//! `None` there, while [`Deref`] panics with a clear message. Code that can
//! legitimately read before the first tick uses `get()`.
//!
//! # The loan budget
//!
//! Every live [`Pooled<T>`] (and every value slot, `delay` queue entry or
//! `accumulate` element holding one) occupies a pool buffer. Size `capacity`
//! for the peak: values in flight **plus** values parked in the graph. A
//! graph that holds handles indefinitely starves [`loan`](PooledSender::loan)
//! — that is the backpressure doing its job, but it means `accumulate` over
//! an unbounded run wants either a generous capacity or a design that
//! extracts owned data (`map` to a summary) instead of parking handles.
//!
//! The **source's own slot** is deliberately exempt from that arithmetic: a
//! burst can hold up to the whole pool (a producer racing the graph's first
//! cycle), so the pooled receiver drops its parked burst on a quiet wake,
//! and an exhausted [`loan`](PooledSender::loan) *causes* that wake (a
//! checkpoint nudge). Downstream holds are the user's ledger; the source's
//! last burst is not.
//!
//! # Residual allocations (deliberate, documented)
//!
//! The pool removes payload allocations. Two small ones remain per message
//! in this first cut, both bounded and both with known closures if profiles
//! demand them: the handle's `Rc` control block (~32B; the end-state fix is
//! a slab handle — `{slot, generation}` indices into a preallocated side
//! table), and the transport queue's internal node. The *payload* — the part
//! that scales with message size — is never allocated or freed at steady
//! state.

use std::fmt;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};

use crate::channel::ChannelSender;
use wingfoil::NanoTime;

/// A writable loan of one pool buffer — the producer-side stage of a pooled
/// value's life. Obtained from [`PooledSender::loan`], written in place
/// (`DerefMut`), then passed to [`PooledSender::send`] /
/// [`send_at`](PooledSender::send_at). Dropping an unsent loan returns the
/// buffer to the pool.
///
/// `Send` (for `T: Send`): the loan is the unique owner, so it crosses
/// threads without any refcount.
pub struct PoolLoan<T> {
    /// `Some` for the loan's whole life; the buffer is taken exactly once, by
    /// [`Drop`] — which is also what runs when the last graph-side
    /// [`Pooled`] handle releases the adopted loan.
    buf: Option<Box<T>>,
    ret: Sender<Box<T>>,
}

impl<T> PoolLoan<T> {
    fn new(buf: Box<T>, ret: Sender<Box<T>>) -> Self {
        Self {
            buf: Some(buf),
            ret,
        }
    }
}

impl<T> Deref for PoolLoan<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.buf
            .as_ref()
            .expect("invariant: loan buffer present until dropped")
    }
}

impl<T> DerefMut for PoolLoan<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.buf
            .as_mut()
            .expect("invariant: loan buffer present until dropped")
    }
}

impl<T> Drop for PoolLoan<T> {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            // The pool (or the whole producer) may already be gone — e.g. the
            // graph drops queued messages after the sender was dropped. The
            // buffer then just deallocates, which is the correct end of life.
            let _ = self.ret.send(buf);
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for PoolLoan<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("PoolLoan").field(&**self).finish()
    }
}

/// The graph-side handle to a pooled value: a **non-atomic** shared reference
/// (`Rc`-based, deliberately `!Send` — the payload crossed threads once, as a
/// unique [`PoolLoan`]; after adoption it lives on the graph thread). `Clone`
/// bumps a refcount and never touches `T`; the last drop returns the buffer
/// to the producer's pool.
///
/// Derefs to `T`. [`Default`] is the *empty* handle (a pre-first-tick slot
/// seed): [`get`](Self::get) returns `None` on it, `Deref` panics with a
/// clear message. `PartialEq` is pointer identity (empty handles compare
/// equal to each other).
pub struct Pooled<T>(Option<Rc<PoolLoan<T>>>);

impl<T> Pooled<T> {
    /// Adopt a loan that has crossed into the graph, wrapping it in the
    /// shared handle. Called by the `pooled_channel` receiver on the graph
    /// thread; public so custom adapters can feed pooled payloads through
    /// their own ingress.
    pub fn adopt(loan: PoolLoan<T>) -> Self {
        Self(Some(Rc::new(loan)))
    }

    /// The payload, or `None` for the empty (pre-first-tick) handle.
    pub fn get(&self) -> Option<&T> {
        self.0.as_deref().map(|loan| &**loan)
    }

    /// True for the default, payload-less handle a value slot is seeded with.
    pub fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

impl<T> Clone for Pooled<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T> Default for Pooled<T> {
    fn default() -> Self {
        Self(None)
    }
}

impl<T> Deref for Pooled<T> {
    type Target = T;
    fn deref(&self) -> &T {
        match self.get() {
            Some(v) => v,
            None => panic!(
                "Pooled::deref on an empty handle — the stream has not ticked yet; \
                 use Pooled::get() where a pre-first-tick (passive) read is possible"
            ),
        }
    }
}

/// Pointer identity: two handles are equal iff they share one loan. Empty
/// handles compare equal. This is what `delay`/`feedback` dedup wants, and it
/// never deep-compares the payload.
impl<T> PartialEq for Pooled<T> {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Rc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        }
    }
}

impl<T: fmt::Debug> fmt::Debug for Pooled<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.get() {
            Some(v) => f.debug_tuple("Pooled").field(v).finish(),
            None => f.write_str("Pooled(<empty>)"),
        }
    }
}

/// The producer half of a [`pooled_channel`]: a bounded buffer pool plus the
/// channel sender. Single producer (deliberately not `Clone` — the free list
/// is unsynchronised; clone-able multi-producer pooling is a follow-on if an
/// adapter needs it). Move it to the producing thread; every send wakes the
/// receiving kernel exactly like [`ChannelSender`].
///
/// [`pooled_channel`]: crate::fluent::SourceOps::pooled_channel
pub struct PooledSender<T> {
    chan: ChannelSender<PoolLoan<T>>,
    /// Buffers ready to loan. Refilled from `ret_rx` on each loan call.
    free: Vec<Box<T>>,
    /// The return queue: the last graph-side handle drop sends the buffer
    /// back here.
    ret_rx: Receiver<Box<T>>,
    /// Template for the per-loan return sender.
    ret_tx: Sender<Box<T>>,
    capacity: usize,
}

impl<T> PooledSender<T> {
    pub(crate) fn new(
        chan: ChannelSender<PoolLoan<T>>,
        free: Vec<Box<T>>,
        ret_rx: Receiver<Box<T>>,
        ret_tx: Sender<Box<T>>,
    ) -> Self {
        let capacity = free.len();
        Self {
            chan,
            free,
            ret_rx,
            ret_tx,
            capacity,
        }
    }

    /// Drain every buffer the graph has returned into the free list.
    fn reclaim(&mut self) {
        while let Ok(buf) = self.ret_rx.try_recv() {
            self.free.push(buf);
        }
    }

    /// On an exhausted pool, wake the receiving graph without sending a
    /// value (a checkpoint). The receiver treats a wake that delivers
    /// nothing as the cue to drop the *previous* burst parked in its value
    /// slot — which may hold up to `capacity` loans if the producer raced
    /// the graph's first cycle. Without this nudge, a flat-out producer and
    /// an otherwise-idle graph deadlock: `loan` waits for returns that only
    /// a new burst (which needs a loan) would have triggered.
    fn nudge(&self) {
        let _ = self.chan.checkpoint(NanoTime::ZERO);
    }

    /// Loan a recycled buffer, **blocking** until one is available if the
    /// full `capacity` is in flight — the pool's backpressure, the moral
    /// equivalent of a bounded channel's blocking `send`. The buffer holds
    /// whatever the previous use left (contents undefined); overwrite it —
    /// for `Vec` fields, `clear()` + fill, which keeps their heap capacity.
    ///
    /// An exhausted pool wakes the graph (see [`nudge`](Self::nudge)) so the
    /// last delivered burst is released even when nothing else would cycle
    /// it. The call still blocks forever if the graph *parks* every handle —
    /// `accumulate` over the whole run, an unbounded `delay` — which is the
    /// loan budget doing its job (see the module docs); use
    /// [`try_loan`](Self::try_loan) where that is a live concern.
    pub fn loan(&mut self) -> PoolLoan<T> {
        self.reclaim();
        let buf = match self.free.pop() {
            Some(buf) => buf,
            // Everything is in flight: wake the graph, then wait for it to
            // release. `recv` cannot error while we hold `ret_tx`, so this
            // is a pure block-until-returned.
            None => {
                self.nudge();
                self.ret_rx
                    .recv()
                    .expect("invariant: pool return queue open while sender holds ret_tx")
            }
        };
        PoolLoan::new(buf, self.ret_tx.clone())
    }

    /// [`loan`](Self::loan) without blocking: `None` when the full capacity
    /// is in flight. An exhausted pool wakes the graph (see
    /// [`nudge`](Self::nudge)); buffers it releases surface on a later call,
    /// once the graph has cycled.
    pub fn try_loan(&mut self) -> Option<PoolLoan<T>> {
        self.reclaim();
        match self.free.pop() {
            Some(buf) => Some(PoolLoan::new(buf, self.ret_tx.clone())),
            None => {
                self.nudge();
                None
            }
        }
    }

    /// Send a written loan (realtime: ticks on arrival; historical: replays
    /// at the run's start time). Returns false once the receiver is gone.
    pub fn send(&self, loan: PoolLoan<T>) -> bool {
        self.chan.send(loan)
    }

    /// Send a written loan stamped with an explicit graph time — the
    /// historical-replay form, exactly [`ChannelSender::send_at`].
    pub fn send_at(&self, loan: PoolLoan<T>, time: NanoTime) -> bool {
        self.chan.send_at(loan, time)
    }

    /// Propagate an error into the receiving graph, aborting its run.
    pub fn send_error(&self, error: anyhow::Error) -> bool {
        self.chan.send_error(error)
    }

    /// A progress marker with no value.
    pub fn checkpoint(&self, time: NanoTime) -> bool {
        self.chan.checkpoint(time)
    }

    /// Signal end-of-stream: no more values will be sent.
    pub fn close(&self) -> bool {
        self.chan.close()
    }

    /// The pool's fixed capacity — the loan budget.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Buffers available to loan right now (reclaims returns first).
    pub fn available(&mut self) -> usize {
        self.reclaim();
        self.free.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool_parts(capacity: usize) -> (Vec<Box<u64>>, Receiver<Box<u64>>, Sender<Box<u64>>) {
        let (ret_tx, ret_rx) = std::sync::mpsc::channel();
        let free = (0..capacity).map(|_| Box::new(0u64)).collect();
        (free, ret_rx, ret_tx)
    }

    /// Dropping an unsent loan returns its buffer to the pool's queue.
    #[test]
    fn dropped_loan_returns_its_buffer() {
        let (mut free, ret_rx, ret_tx) = pool_parts(1);
        let loan = PoolLoan::new(free.pop().unwrap(), ret_tx);
        drop(loan);
        assert!(ret_rx.try_recv().is_ok(), "buffer back on the return queue");
    }

    /// The last handle drop — not the first — releases the buffer.
    #[test]
    fn last_handle_drop_releases() {
        let (mut free, ret_rx, ret_tx) = pool_parts(1);
        let mut loan = PoolLoan::new(free.pop().unwrap(), ret_tx);
        *loan = 7;
        let a = Pooled::adopt(loan);
        let b = a.clone();
        drop(a);
        assert!(
            ret_rx.try_recv().is_err(),
            "still one live handle — buffer must stay out"
        );
        assert_eq!(Some(&7), b.get());
        drop(b);
        assert!(ret_rx.try_recv().is_ok(), "last drop returns the buffer");
    }

    /// Handle equality is pointer identity; empty handles compare equal.
    #[test]
    fn pooled_equality_is_pointer_identity() {
        let (mut free, _ret_rx, ret_tx) = pool_parts(2);
        let a = Pooled::adopt(PoolLoan::new(free.pop().unwrap(), ret_tx.clone()));
        let b = Pooled::adopt(PoolLoan::new(free.pop().unwrap(), ret_tx));
        assert_eq!(a, a.clone(), "same loan: equal");
        assert_ne!(a, b, "distinct loans: unequal, even with equal payloads");
        assert_eq!(Pooled::<u64>::default(), Pooled::<u64>::default());
        assert_ne!(a, Pooled::<u64>::default());
    }

    /// The default handle is empty: `get` is `None`, `is_empty` is true.
    #[test]
    fn default_handle_is_empty() {
        let p = Pooled::<u64>::default();
        assert!(p.is_empty());
        assert_eq!(None, p.get());
    }

    #[test]
    #[should_panic(expected = "Pooled::deref on an empty handle")]
    fn default_handle_deref_panics_with_context() {
        let p = Pooled::<u64>::default();
        let _ = *p;
    }
}
