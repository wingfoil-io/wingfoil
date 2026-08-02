use crate::types::NanoTime;
use derive_new::new;
use serde::{Deserialize, Serialize};
use std::cmp::Eq;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};

/// A value emitted at, or captured at a specific time.
#[doc(hidden)]
#[derive(Debug, Clone, new, Default, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn hash_of<T: Hash>(val: &T) -> u64 {
        let mut h = DefaultHasher::new();
        val.hash(&mut h);
        h.finish()
    }

    #[test]
    fn equal_value_at_instances_are_equal() {
        let a = ValueAt::new(42u64, NanoTime::new(100));
        let b = ValueAt::new(42u64, NanoTime::new(100));
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn different_value_not_equal() {
        let a = ValueAt::new(1u64, NanoTime::new(100));
        let b = ValueAt::new(2u64, NanoTime::new(100));
        assert_ne!(a, b);
    }

    #[test]
    fn different_time_not_equal() {
        let a = ValueAt::new(1u64, NanoTime::new(100));
        let b = ValueAt::new(1u64, NanoTime::new(200));
        assert_ne!(a, b);
    }

    #[test]
    fn default_is_zero_time_and_default_value() {
        let v: ValueAt<u64> = ValueAt::default();
        assert_eq!(v.value, 0);
        assert_eq!(v.time, NanoTime::new(0));
    }
}
