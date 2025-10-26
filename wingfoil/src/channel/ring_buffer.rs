use ringbuf::{HeapRb, HeapProd, HeapCons};
use ringbuf::traits::{Consumer, Producer, Split};

use derive_more::Debug;

use std::sync::Arc;
use derive_new::new;
use event_listener::{Event, Listener}; // runtime-agnostic notify/wait

#[derive(Debug)]
pub struct HeapRingBuf<T> {
    #[debug(skip)]
    rb: HeapRb<T>,
}

impl<T> HeapRingBuf<T> {
    pub fn new(capacity: usize) -> Self {
        Self { rb: HeapRb::new(capacity) }
    }

    pub fn split(self) -> (ProducerEnd<T>, ConsumerEnd<T>) {
        let (prod, cons) = self.rb.split();
        let ev = Arc::new(Event::new());
        (
            ProducerEnd::new(prod, ev.clone()),
            ConsumerEnd::new(cons, ev),
        )
    }
}

#[derive(new, Debug)]
pub struct ProducerEnd<T> {
    #[debug(skip)]
    prod: HeapProd<T>,
    #[debug(skip)]
    ev: Arc<Event>,
}

#[derive(new, Debug)]
pub struct ConsumerEnd<T> {
    #[debug(skip)]    
    cons: HeapCons<T>,
    #[debug(skip)]
    ev: Arc<Event>,
}

impl<T:std::fmt::Debug> ProducerEnd<T> {
    /// Non-blocking push; on success, notify one waiter.
    pub fn push(&mut self, value: T)  {
        self.prod.try_push(value).unwrap();
        // Wake exactly one waiter (thread or async task).
        self.ev.notify(1);
    }
}

impl<T> ConsumerEnd<T> {
    /// Non-blocking pop
    pub fn pop(&mut self) -> Option<T> {
        self.cons.try_pop()
    }

    /// **Blocking** pop with no Tokio dependency.
    /// Waits efficiently until an item is available.
    pub fn pop_blocking(&mut self) -> T {
        loop {
            if let Some(v) = self.pop() {
                return v;
            }
            // Create a listener *before* re-checking to avoid races.
            let listener = self.ev.listen();
            // Re-check to avoid sleeping if something arrived meanwhile.
            if let Some(v) = self.pop() {
                return v;
            }
            // Park the current thread until notified; may spuriously wake.
            listener.wait();
            // loop and try again
        }
    }

    /// Optional async pop that is **runtime-agnostic** (no Tokio tie-in).
    /// Works on any executor because `event-listener` integrates with Wakers.
    pub async fn pop_async(&mut self) -> T {
        loop {
            if let Some(v) = self.pop() {
                return v;
            }
            let listener = self.ev.listen();
            if let Some(v) = self.pop() {
                return v;
            }
            listener.await; // await notification
        }
    }
}
