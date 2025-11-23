use pyo3::prelude::*;
use pyo3::types::PyAny;

pub struct PyElement(Option<Py<PyAny>>);

impl PyElement {
    pub fn none() -> Self {
        PyElement(None)
    }

    pub fn new(val: Py<PyAny>) -> Self {
        PyElement(Some(val))
    }

    pub fn as_ref(&self) -> &Py<PyAny> {
        self.0.as_ref().unwrap()
    }

    pub fn value(&self) -> Py<PyAny> {
        Python::attach(|py| self.as_ref().clone_ref(py))
    }
}

impl Default for PyElement {
    fn default() -> Self {
        Python::attach(|py| PyElement(Some(py.None())))
    }
}

impl std::fmt::Debug for PyElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Python::attach(|py| {
            let res = self
                .as_ref()
                .call_method0(py, "__str__")
                .unwrap()
                .extract::<String>(py)
                .unwrap();
            write!(f, "{}", res)
        })
    }
}

impl Clone for PyElement {
    fn clone(&self) -> Self {
        match &self.0 {
            Some(inner) => Python::attach(|py| PyElement(Some(inner.clone_ref(py)))),
            None => PyElement(None),
        }
    }
}

impl std::ops::Not for PyElement {
    type Output = PyElement;

    fn not(self) -> Self::Output {
        Python::attach(|py| {
            let res = self.as_ref().call_method0(py, "__neg__").unwrap();
            PyElement::new(res)
        })
    }
}

impl std::ops::Add for PyElement {
    type Output = PyElement;

    fn add(self, rhs: PyElement) -> Self::Output {
        Python::attach(|py| {
            let res = self
                .as_ref()
                .call_method1(py, "__add__", (rhs.as_ref(),))
                .unwrap();
            PyElement::new(res)
        })
    }
}

impl std::ops::Sub for PyElement {
    type Output = PyElement;

    fn sub(self, rhs: PyElement) -> Self::Output {
        Python::attach(|py| {
            let res = self
                .as_ref()
                .call_method1(py, "__sub__", (rhs.as_ref(),))
                .unwrap();
            PyElement::new(res)
        })
    }
}

impl PartialEq for PyElement {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => {
                Python::attach(|py| {
                    // keep cloned Py<T> alive outside bind()
                    let a_obj = a.clone_ref(py);
                    let b_obj = b.clone_ref(py);

                    // bind inside attach so temporary doesn't drop too early
                    let a_bound = a_obj.bind(py);
                    let b_bound = b_obj.bind(py);

                    match a_bound.rich_compare(b_bound, pyo3::basic::CompareOp::Eq) {
                        Ok(obj) => obj.is_truthy().unwrap_or(false),
                        Err(_) => false,
                    }
                })
            }
            (None, None) => true,
            _ => false,
        }
    }
}

impl Eq for PyElement {}

use std::hash::{Hash, Hasher};

impl Hash for PyElement {
    fn hash<H: Hasher>(&self, state: &mut H) {
        match &self.0 {
            Some(obj) => {
                Python::attach(|py| {
                    let obj_ref = obj.clone_ref(py);
                    let bound = obj_ref.bind(py);
                    match bound.hash() {
                        Ok(hash_val) => state.write_isize(hash_val),
                        Err(err) => panic!("hash failed, {err:?}"),
                    }
                });
            }
            None => {
                panic!("can not hash empty PyElement")
            }
        }
    }
}
