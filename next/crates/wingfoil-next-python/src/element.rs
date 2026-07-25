//! [`PyElement`] — the erased value that flows on Python-composable edges.
//!
//! A `PyElement` wraps an owned Python object (`Py<PyAny>`), or `None` for the
//! default/empty slot the engine seeds every value store with. Every operation
//! that touches the Python object reacquires the GIL via `Python::attach`, so a
//! `PyElement` is safe to hold and move on the Rust side between cycles.

use anyhow::{Context, Result};
use pyo3::BoundObject;
use pyo3::prelude::*;
use pyo3::types::PyAny;

/// An erased Python value flowing on a wingfoil-next edge.
///
/// The `None` variant is the engine's `Default`: fresh value slots start empty
/// and are overwritten on the owning node's first tick, exactly as a typed slot
/// starts at `T::default()`.
#[derive(Default)]
pub struct PyElement(Option<Py<PyAny>>);

impl PyElement {
    /// The empty element — the `Default`, before a node has produced a value.
    pub fn none() -> Self {
        PyElement(None)
    }

    /// Wrap an owned Python object.
    pub fn new(val: Py<PyAny>) -> Self {
        PyElement(Some(val))
    }

    /// Whether this element is the empty (`None`) placeholder.
    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }

    /// Borrow the inner Python object.
    ///
    /// # Panics
    ///
    /// Panics if called on an empty element. Callers that can see the `None`
    /// case (edge conversions) go through the checked paths below instead; this
    /// documents the precondition.
    pub fn object(&self) -> &Py<PyAny> {
        self.0
            .as_ref()
            .expect("invariant: object() called on an empty (None) PyElement")
    }

    /// Clone out the inner object for handing back to Python.
    ///
    /// # Panics
    ///
    /// Panics on an empty element (see [`object`](Self::object)).
    pub fn value(&self) -> Py<PyAny> {
        Python::attach(|py| self.object().clone_ref(py))
    }
}

impl std::fmt::Debug for PyElement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            None => write!(f, "None"),
            Some(obj) => Python::attach(|py| {
                match obj
                    .call_method0(py, "__str__")
                    .and_then(|s| s.extract::<String>(py))
                {
                    Ok(res) => write!(f, "{res}"),
                    Err(err) => write!(f, "<PyElement __str__ failed: {err}>"),
                }
            }),
        }
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

impl PartialEq for PyElement {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Python::attach(|py| {
                let a_bound = a.bind(py);
                let b_bound = b.bind(py);
                match a_bound.rich_compare(b_bound, pyo3::basic::CompareOp::Eq) {
                    Ok(obj) => obj.is_truthy().unwrap_or(false),
                    Err(_) => false,
                }
            }),
            (None, None) => true,
            _ => false,
        }
    }
}

impl std::ops::Not for PyElement {
    type Output = PyElement;

    fn not(self) -> Self::Output {
        Python::attach(|py| {
            let res = self
                .object()
                .call_method0(py, "__neg__")
                .expect("invariant: PyElement value must support __neg__ for `not`/`!`");
            PyElement::new(res)
        })
    }
}

impl std::ops::Add for PyElement {
    type Output = PyElement;

    fn add(self, rhs: PyElement) -> Self::Output {
        Python::attach(|py| {
            let res = self
                .object()
                .call_method1(py, "__add__", (rhs.object(),))
                .expect("invariant: PyElement value must support __add__ for `+`/`sum`");
            PyElement::new(res)
        })
    }
}

impl std::ops::Sub for PyElement {
    type Output = PyElement;

    fn sub(self, rhs: PyElement) -> Self::Output {
        Python::attach(|py| {
            let res = self
                .object()
                .call_method1(py, "__sub__", (rhs.object(),))
                .expect("invariant: PyElement value must support __sub__ for `-`/`difference`");
            PyElement::new(res)
        })
    }
}

// ---------------------------------------------------------------------------
// Edge conversions.
//
// Only the seams convert. `From<..>` boxes a Rust scalar the way an op that
// *produces* a Python-facing value would; `TryFrom<&PyElement>` extracts the
// scalar an op *consumes*. Extraction is fallible (the Python object may be the
// wrong type, or empty), construction is not.
// ---------------------------------------------------------------------------

impl From<Py<PyAny>> for PyElement {
    fn from(val: Py<PyAny>) -> Self {
        PyElement(Some(val))
    }
}

impl From<Bound<'_, PyAny>> for PyElement {
    fn from(val: Bound<'_, PyAny>) -> Self {
        PyElement(Some(val.unbind()))
    }
}

/// Box a Rust scalar into a Python object. Primitive `IntoPyObject`
/// conversions are infallible, so the `expect` is an unreachable invariant.
fn boxed<'py, T>(py: Python<'py>, value: T) -> Py<PyAny>
where
    T: IntoPyObject<'py>,
{
    value
        .into_pyobject(py)
        .map(|b| b.into_any().unbind())
        .map_err(Into::into)
        .expect("invariant: scalar -> PyObject conversion is infallible")
}

macro_rules! from_scalar {
    ($($t:ty),* $(,)?) => {$(
        impl From<$t> for PyElement {
            fn from(value: $t) -> Self {
                Python::attach(|py| PyElement(Some(boxed(py, value))))
            }
        }
    )*};
}
from_scalar!(f64, i64, bool);

impl From<String> for PyElement {
    fn from(value: String) -> Self {
        Python::attach(|py| PyElement(Some(boxed(py, value))))
    }
}

impl From<&str> for PyElement {
    fn from(value: &str) -> Self {
        Python::attach(|py| PyElement(Some(boxed(py, value))))
    }
}

macro_rules! try_into_scalar {
    ($($t:ty),* $(,)?) => {$(
        impl TryFrom<&PyElement> for $t {
            type Error = anyhow::Error;

            fn try_from(el: &PyElement) -> Result<Self> {
                let obj = el
                    .0
                    .as_ref()
                    .context(concat!("cannot extract ", stringify!($t), " from an empty PyElement"))?;
                Python::attach(|py| {
                    obj.extract::<$t>(py)
                        .with_context(|| format!("PyElement is not a {}", stringify!($t)))
                })
            }
        }
    )*};
}
try_into_scalar!(f64, i64, bool, String);

#[cfg(test)]
mod tests {
    use super::*;
    use wingfoil::{NanoTime, RunFor, RunMode};
    use wingfoil_next::prelude::*;

    #[test]
    fn default_is_empty() {
        assert!(PyElement::default().is_none());
        assert_eq!(PyElement::default(), PyElement::none());
    }

    #[test]
    fn scalar_round_trips() {
        let e = PyElement::from(21.0_f64);
        let back: f64 = (&e).try_into().unwrap();
        assert_eq!(21.0, back);
    }

    #[test]
    fn arithmetic_delegates_to_python() {
        let sum = PyElement::from(2.0_f64) + PyElement::from(3.0_f64);
        let v: f64 = (&sum).try_into().unwrap();
        assert_eq!(5.0, v);
    }

    #[test]
    fn equality_uses_python_semantics() {
        assert_eq!(PyElement::from(1.0_f64), PyElement::from(1.0_f64));
        assert_ne!(PyElement::from(1.0_f64), PyElement::from(2.0_f64));
    }

    #[test]
    fn extract_wrong_type_errors() {
        let e = PyElement::from("hello");
        let r: Result<f64> = (&e).try_into();
        assert!(r.is_err());
    }

    /// The load-bearing test: `PyElement` satisfies next's stream-value bounds
    /// and flows through the interpreted engine, computing on the Python side,
    /// with the right value at the right tick.
    #[test]
    fn flows_through_next_graph() {
        let g = GraphBuilder::new();
        let src = g.constant(PyElement::from(21.0_f64));
        let doubled = src.map(|e: &PyElement| e.clone() + e.clone());

        let mut runner = g.build();
        runner
            .run(RunMode::HistoricalFrom(NanoTime::ZERO), RunFor::Cycles(1))
            .unwrap();

        let out: f64 = (&runner.value(&doubled)).try_into().unwrap();
        assert_eq!(42.0, out);
    }
}
