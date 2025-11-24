from . import _wingfoil as _ext
from ._wingfoil import *
from .stream import *
__doc__ = _ext.__doc__
__version__ = getattr(_ext, "__version__", None)
__all__ = _ext.__all__