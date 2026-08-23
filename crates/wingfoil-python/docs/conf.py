"""Sphinx configuration for the ``wingfoil`` Python bindings.

The documented module is ``wingfoil`` — a *mixed* maturin layout: the
importable package lives in ``../python/wingfoil`` and wraps the compiled
extension ``wingfoil._wingfoil`` (see ``../pyproject.toml``). Autodoc
therefore needs the **compiled** extension importable, not just the source
tree: ``maturin develop`` from ``..``, or a wheel install, before ``make html``.

Two things follow from that layout and are worth knowing before editing this
file:

* The pure-Python package under ``../python`` is added to ``sys.path`` so a
  source checkout can be documented without an editable install, but that alone
  is not enough — ``wingfoil/__init__.py`` imports ``._wingfoil``, which
  only exists once the crate has been built.
* Adapter bindings are **cargo-feature-gated**, so the set of module-level
  functions autodoc sees depends on the features the extension was built with.
  ``api.rst`` therefore keeps a hand-written index of the full surface and uses
  autosummary only for what the build actually carries; a docs build with fewer
  features renders a smaller generated table, not an error.
"""

import os
import sys

# The pure-Python half of the mixed layout (`python-source = "python"`).
sys.path.insert(0, os.path.abspath("../python"))

# -- Project information -----------------------------------------------------

project = "wingfoil-python"
copyright = "2026, Jake Mitchell"
author = "Jake Mitchell"


def _release() -> str:
    """The version to stamp the build with.

    Prefer the installed distribution's metadata (what maturin wrote from
    ``Cargo.toml``), fall back to the module's own ``__version__``, and finally
    to the crate version so a bare source checkout still builds.
    """
    try:
        from importlib.metadata import PackageNotFoundError, version

        try:
            # The distribution is `wingfoil` (see pyproject.toml); only the
            # crate is `wingfoil-python`. Looking the crate name up here found
            # nothing and fell through to the module `__version__`, so the
            # docs stamped a version without anyone noticing.
            return version("wingfoil")
        except PackageNotFoundError:
            pass
    except ImportError:  # pragma: no cover - Python < 3.8
        pass
    try:
        import wingfoil

        return getattr(wingfoil, "__version__", None) or "0.1.0"
    except ImportError:
        return "0.1.0"


release = _release()
version = release

# -- Import check ------------------------------------------------------------
#
# Fail with an actionable message rather than a wall of autodoc import errors.
# On Read the Docs the package is pip-installed (maturin builds the extension
# as part of that), so this branch is a local-build guard.
try:
    import wingfoil  # noqa: F401
except ImportError as exc:  # pragma: no cover - build-environment guard
    raise SystemExit(
        "cannot import `wingfoil` — the compiled extension is not built.\n"
        "Build it first:\n"
        "    cd crates/wingfoil-python && maturin develop\n"
        f"(underlying error: {exc})"
    ) from exc

# -- General configuration ---------------------------------------------------

extensions = [
    "sphinx.ext.autodoc",  # REQUIRED: extract docstrings (incl. pyo3 ones)
    "sphinx.ext.autosummary",  # generate the API tables
    "sphinx.ext.napoleon",  # Google/NumPy style docstrings
    "sphinx.ext.viewcode",  # source links for the pure-Python surface
    "myst_parser",  # the crate README, included by `readme.rst`
]

templates_path = ["_templates"]
# `README.md` documents this folder for contributors; it is not a page. It has
# to be excluded explicitly because myst_parser registers `.md` as a source
# suffix, which would otherwise pull it in as an orphan document.
exclude_patterns = ["_build", "README.md", "Thumbs.db", ".DS_Store"]

# -- Autodoc / autosummary ---------------------------------------------------

autodoc_default_options = {
    "members": True,
    "undoc-members": True,
    "show-inheritance": True,
}
# pyo3 classes are not introspectable the way pure-Python ones are; keep the
# signature out of the heading and let the docstring carry it.
autodoc_typehints = "description"
autosummary_generate = True
add_module_names = False

# MyST: the README uses fenced code and tables.
myst_enable_extensions = ["colon_fence", "deflist"]
# The README is a top-level document included via `readme.rst`; suppress the
# duplicate-header anchors MyST would otherwise warn about.
myst_heading_anchors = 3

# -- Options for HTML output -------------------------------------------------

html_theme = "sphinx_rtd_theme"
html_static_path = ["_static"]
