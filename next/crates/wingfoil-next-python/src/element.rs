//! [`PyElement`] — the erased value that flows on Python-composable edges.
//!
//! A `PyElement` wraps an owned Python object (`Py<PyAny>`), or `None` for the
//! default/empty slot the engine seeds every value store with. Every operation
//! that touches the Python object reacquires the GIL via `Python::attach`, so a
//! `PyElement` is safe to hold and move on the Rust side between cycles.

use anyhow::{Context, Result};
use pyo3::BoundObject;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes};

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

    /// Box a slice of elements into a Python `list` — the edge conversion for
    /// the collection ops (`accumulate`/`buffer`/`window`), which produce a
    /// `Vec<PyElement>` that must cross into Python-composable space as one
    /// value. Empty (`None`) members become Python `None`.
    pub fn list(items: &[PyElement]) -> Self {
        Python::attach(|py| {
            let objects = items.iter().map(|item| match &item.0 {
                Some(obj) => obj.clone_ref(py),
                None => py.None(),
            });
            let list = pyo3::types::PyList::new(py, objects)
                .expect("invariant: PyList::new from an exact-size iterator of owned objects");
            PyElement::new(list.into_any().unbind())
        })
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
from_scalar!(
    f32, f64, i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, bool,
);

/// `Vec<u8>` erases to Python **`bytes`**, not a list of ints — the shape a
/// binary payload wants (pyo3's blanket `Vec<T>` conversion would give a
/// `list[int]`, which is both wrong-typed and far slower for a message body).
impl From<Vec<u8>> for PyElement {
    fn from(value: Vec<u8>) -> Self {
        Python::attach(|py| PyElement(Some(PyBytes::new(py, &value).into_any().unbind())))
    }
}

/// `Option<T>` erases with `None` as Python `None`, so a nullable field crosses
/// the boundary without the adapter hand-rolling the empty case.
impl<T: Into<PyElement>> From<Option<T>> for PyElement {
    fn from(value: Option<T>) -> Self {
        match value {
            Some(v) => v.into(),
            None => PyElement::none(),
        }
    }
}

/// The unit type erases to Python `None` — so a **sink** adapter that produces a
/// `Stream<()>` (a terminal, no meaningful value) can cross the boundary like
/// any other, its Python-facing value simply being `None`.
impl From<()> for PyElement {
    fn from((): ()) -> Self {
        PyElement::none()
    }
}

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
try_into_scalar!(
    f32, f64, i8, i16, i32, i64, isize, u8, u16, u32, u64, usize, bool, String,
);

/// The inverse of the `bytes` edge. Accepts `bytes`/`bytearray` (and, via
/// pyo3's extraction, a sequence of ints) so a Python caller can hand back
/// whichever it has.
impl TryFrom<&PyElement> for Vec<u8> {
    type Error = anyhow::Error;

    fn try_from(el: &PyElement) -> Result<Self> {
        let obj =
            el.0.as_ref()
                .context("cannot extract Vec<u8> from an empty PyElement")?;
        Python::attach(|py| {
            obj.extract::<Vec<u8>>(py)
                .context("PyElement is not bytes-like")
        })
    }
}

/// The inverse of the `Option<T>` edge: Python `None` (or an empty element)
/// reads back as `None`, anything else as `Some`.
impl<T> TryFrom<&PyElement> for Option<T>
where
    T: for<'a> TryFrom<&'a PyElement, Error = anyhow::Error>,
{
    type Error = anyhow::Error;

    fn try_from(el: &PyElement) -> Result<Self> {
        if el.is_none() {
            return Ok(None);
        }
        T::try_from(el).map(Some)
    }
}

/// The **identity** edge conversion: extracting a `PyElement` from a
/// `PyElement` is a clone.
///
/// This is what lets an adapter whose payload is inherently *dynamic* — a
/// PostgreSQL row written from an arbitrary Python `dict`, say — stay on the
/// erased type at the seam and do its own marshaling inside the adapter (where
/// the column spec is in scope), while still going through the standard
/// `#[pyadapter]` `typed_input` / `typed_burst_input` path. Without it such an
/// adapter would need a hand-written `#[pyfunction]`.
impl TryFrom<&PyElement> for PyElement {
    type Error = anyhow::Error;

    fn try_from(el: &PyElement) -> Result<Self> {
        Ok(el.clone())
    }
}

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

    #[test]
    fn integer_widths_round_trip() {
        // Every width goes out and comes back; the narrow ones matter because a
        // record field is rarely an i64.
        assert_eq!(7_u8, u8::try_from(&PyElement::from(7_u8)).unwrap());
        assert_eq!(7_u16, u16::try_from(&PyElement::from(7_u16)).unwrap());
        assert_eq!(7_u32, u32::try_from(&PyElement::from(7_u32)).unwrap());
        assert_eq!(7_u64, u64::try_from(&PyElement::from(7_u64)).unwrap());
        assert_eq!(7_usize, usize::try_from(&PyElement::from(7_usize)).unwrap());
        assert_eq!(-7_i8, i8::try_from(&PyElement::from(-7_i8)).unwrap());
        assert_eq!(-7_i16, i16::try_from(&PyElement::from(-7_i16)).unwrap());
        assert_eq!(-7_i32, i32::try_from(&PyElement::from(-7_i32)).unwrap());
        assert_eq!(
            -7_isize,
            isize::try_from(&PyElement::from(-7_isize)).unwrap()
        );
    }

    #[test]
    fn f32_round_trips() {
        assert_eq!(1.5_f32, f32::try_from(&PyElement::from(1.5_f32)).unwrap());
    }

    #[test]
    fn a_negative_value_does_not_silently_become_unsigned() {
        let element = PyElement::from(-1_i64);
        assert!(
            u32::try_from(&element).is_err(),
            "a negative must not wrap into an unsigned width"
        );
    }

    #[test]
    fn bytes_erase_to_python_bytes_not_a_list() {
        let element = PyElement::from(vec![1_u8, 2, 255]);
        Python::attach(|py| {
            let obj = element.object().bind(py);
            assert!(
                obj.is_instance_of::<PyBytes>(),
                "Vec<u8> must erase to bytes, got {obj:?}"
            );
        });
        assert_eq!(vec![1_u8, 2, 255], Vec::<u8>::try_from(&element).unwrap());
    }

    #[test]
    fn empty_bytes_round_trip() {
        let element = PyElement::from(Vec::<u8>::new());
        assert_eq!(Vec::<u8>::new(), Vec::<u8>::try_from(&element).unwrap());
    }

    #[test]
    fn option_maps_none_to_python_none() {
        let empty = PyElement::from(None::<f64>);
        assert!(empty.is_none());
        assert_eq!(None, Option::<f64>::try_from(&empty).unwrap());

        let full = PyElement::from(Some(2.5_f64));
        assert!(!full.is_none());
        assert_eq!(Some(2.5), Option::<f64>::try_from(&full).unwrap());
    }

    #[test]
    fn option_propagates_a_wrong_typed_inner() {
        // Present but not extractable is an error, not a silent None — that
        // distinction is the whole point of the nullable edge.
        let text = PyElement::from("not a number");
        assert!(Option::<f64>::try_from(&text).is_err());
    }
}
