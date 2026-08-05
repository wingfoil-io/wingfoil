//! Guard for the link-time binding registration (see
//! [`wingfoil_python::PyFnRegistrar`]).
//!
//! Bindings no longer appear in a hand-written list inside the `#[pymodule]` —
//! `#[pyop]`, `#[pygraph]`, `#[pyadapter]` and `pyop_fn!` each emit a
//! submission beside the function they generate, and the module iterates what
//! was collected. That removes the "forgot the second mention" failure mode,
//! but replaces it with a quieter one: if collection ever stopped working, the
//! module would come up **empty of functions** and every `add_function` call
//! that used to be here would be gone, with nothing to fail at compile time.
//!
//! The pytest suite would notice (it calls the bindings), but only after a
//! wheel build, and the failure would read as "wingfoil has no attribute
//! `csv_read`" rather than pointing here. This test names the mechanism
//! directly.
//!
//! **Scope.** This runs against the *rlib*, so it proves the submissions exist
//! and are collected in an ordinary Rust binary. The shipped artefact is the
//! **cdylib**, where collection depends on the linker keeping the section — a
//! genuinely different question, and one only an actual module import can
//! answer. `crates/wingfoil-python/tests/test_interop.py` and its neighbours
//! are what cover that, by importing the built extension and calling the
//! bindings.

use pyo3::types::{PyDictMethods, PyModuleMethods};
use wingfoil_python::{PyFnRegistrar, inventory};

/// Collection yields the bindings this crate defines.
///
/// The floor is deliberately loose — this is not an inventory of the catalog,
/// it is a check that link-time collection happens at all. A default-feature
/// build carries the built-in ops, the `#[pygraph]` islands, the latency
/// surface and the demo ops; adapters add more, and are feature-gated, so an
/// exact count would be a maintenance burden that fails for the wrong reason.
#[test]
fn bindings_are_collected() {
    let n = inventory::iter::<PyFnRegistrar>().count();
    assert!(
        n >= 15,
        "link-time collection produced only {n} binding registrars. Every \
         Python binding registers itself this way — `#[pyop]` / `#[pygraph]` / \
         `#[pyadapter]` / `pyop_fn!` emit the submission, and a hand-written \
         `#[pyfunction]` calls `register_pyfn!`. A count this low means \
         collection is broken, not that the catalog shrank, and the shipped \
         module would be missing its functions entirely."
    );
}

/// Every registrar is callable, and none of them panics or errors when run
/// against a real module.
///
/// A registrar is a function pointer built by macro expansion; running the
/// whole set here is the cheapest way to catch one that expanded into
/// something that cannot actually register (a duplicate name, say), rather
/// than discovering it at interpreter start-up.
#[test]
fn every_registrar_registers_cleanly() {
    pyo3::Python::initialize();
    pyo3::Python::attach(|py| {
        let m = pyo3::types::PyModule::new(py, "registration_probe")
            .expect("invariant: creating an empty module cannot fail");
        for (i, r) in inventory::iter::<PyFnRegistrar>().enumerate() {
            (r.0)(&m).unwrap_or_else(|e| {
                panic!("binding registrar #{i} failed against a fresh module: {e}")
            });
        }
        let n = inventory::iter::<PyFnRegistrar>().count();
        let listed = m.dict().len();
        assert!(
            listed >= n,
            "{n} registrars ran but the module ended up with only {listed} \
             attributes — two bindings are registering under the same name, so \
             one is silently shadowing the other"
        );
    });
}
