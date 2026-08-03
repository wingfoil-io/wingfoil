"""wingfoil — Python bindings.

`wingfoil` is a thin Python package over the compiled extension
(`wingfoil._wingfoil`): everything the extension exports is re-exported
here unchanged, plus the pure-Python surface that is more naturally written in
Python than through pyo3 — currently :class:`CustomStream`.

The re-export is derived from the extension rather than hand-listed, so a new
Rust-side class, op or adapter binding shows up here the moment it is
registered in the `#[pymodule]` — nothing to keep in sync.
"""

from . import _wingfoil as _ext
from ._wingfoil import *  # noqa: F401,F403 — the engine surface, re-exported
from .stream import CustomStream

__doc__ = getattr(_ext, "__doc__", __doc__)
__version__ = getattr(_ext, "__version__", None)

# Derived, not hand-maintained: whatever the extension exposes, plus our own.
__all__ = [name for name in dir(_ext) if not name.startswith("_")] + ["CustomStream"]
