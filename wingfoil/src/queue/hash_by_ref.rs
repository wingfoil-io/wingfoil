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
        (Rc::as_ptr(&self.val) as *const ()).hash(state);
    }
}

impl<T: ?Sized> PartialEq for HashByRef<T> {
    fn eq(&self, other: &Self) -> bool {
        std::ptr::eq(
            Rc::as_ptr(&self.val) as *const (),
            Rc::as_ptr(&other.val) as *const (),
        )
    }
}
impl<T: ?Sized> Eq for HashByRef<T> {}