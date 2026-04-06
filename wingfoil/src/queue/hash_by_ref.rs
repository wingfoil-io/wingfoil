// lifted from..
// https://github.com/eqv/hash_by_ref
// Copyright (c) 2017 Cornelius Aschermann
// modified to make val pub and add ?Sized constraints

use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Debug)]
pub struct HashByRef<T: ?Sized> {
    pub val: Rc<T>,
}

impl<T: ?Sized> HashByRef<T> {
    pub fn new(val: Rc<T>) -> Self {
        HashByRef { val }
    }
}

impl<T: ?Sized> Hash for HashByRef<T> {
    fn hash<H>(&self, state: &mut H)
    where
        H: Hasher,
    {
        let ptr = Rc::into_raw(self.val.clone());
        ptr.hash(state);
        let _ = unsafe { Rc::from_raw(ptr) };
    }
}

impl<T: ?Sized> PartialEq for HashByRef<T> {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.val, &other.val)
    }
}
impl<T: ?Sized> Eq for HashByRef<T> {}

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
    fn same_rc_are_equal_and_same_hash() {
        let rc: Rc<u64> = Rc::new(42);
        let a = HashByRef::new(rc.clone());
        let b = HashByRef::new(rc.clone());
        assert_eq!(a, b);
        assert_eq!(hash_of(&a), hash_of(&b));
    }

    #[test]
    fn different_rc_with_equal_content_are_not_equal() {
        let a = HashByRef::new(Rc::new(42u64));
        let b = HashByRef::new(Rc::new(42u64));
        assert_ne!(a, b);
    }

    #[test]
    fn clone_of_rc_is_equal() {
        let rc = Rc::new(7u64);
        let a = HashByRef::new(rc.clone());
        let b = HashByRef::new(Rc::clone(&rc));
        assert_eq!(a, b);
    }
}
