from . import _wingfoil as _ext
from ._wingfoil import *
from .stream import *

from .pandas_helpers import to_dataframe, stream_to_dataframe

__doc__ = _ext.__doc__
__version__ = getattr(_ext, "__version__", None)

__all__ = list(_ext.__all__) + ["to_dataframe", "stream_to_dataframe"]

# User-friendly aliases for KDB+ functions
kdb_read = _ext.py_kdb_read
kdb_write = _ext.py_kdb_write