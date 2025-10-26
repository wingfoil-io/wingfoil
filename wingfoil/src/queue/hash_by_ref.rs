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
