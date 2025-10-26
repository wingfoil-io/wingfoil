use crate::types::NanoTime;
use derive_new::new;
use std::cmp::Eq;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};

/// A value emitted at, or captured at a specific time.
#[doc(hidden)]
#[derive(Debug, Clone, new, Default)]
pub struct ValueAt<T> {
    pub value: T,
    pub time: NanoTime,
}

impl<T: Hash> Hash for ValueAt<T> {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        self.value.hash(state);
        self.time.hash(state);
    }
}

impl<T: PartialEq> PartialEq for ValueAt<T> {
    fn eq(&self, other: &Self) -> bool {
        T::eq(&self.value, &other.value) && NanoTime::eq(&self.time, &other.time)
    }
}

impl<T: PartialEq> Eq for ValueAt<T> {}
