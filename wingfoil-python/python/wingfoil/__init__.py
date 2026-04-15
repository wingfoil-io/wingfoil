from . import _wingfoil as _ext


from .stream import *

from ._wingfoil import *
from ._wingfoil import Graph 

from .pandas_helpers import to_dataframe, build_dataframe

__doc__ = getattr(_ext, "__doc__", "")
__version__ = getattr(_ext, "__version__", None)

__all__ = list(getattr(_ext, "__all__", [])) + ["to_dataframe", "build_dataframe"]

# Latency measurement classes
Latency = _ext.Latency
TracedBytes = _ext.TracedBytes
__all__.extend(["Latency", "TracedBytes"])

# User-friendly aliases for CSV functions
csv_read = _ext.py_csv_read

# User-friendly aliases for etcd functions
etcd_sub = _ext.py_etcd_sub

# User-friendly aliases for ZMQ functions
zmq_sub = _ext.py_zmq_sub
zmq_sub_etcd = getattr(_ext, "py_zmq_sub_etcd", None)

# User-friendly aliases for KDB+ functions
kdb_read = _ext.py_kdb_read
kdb_write = _ext.py_kdb_write

# User-friendly aliases for iceoryx2 functions (feature-gated)
if hasattr(_ext, "py_iceoryx2_sub"):
    iceoryx2_sub = _ext.py_iceoryx2_sub
    Iceoryx2ServiceVariant = _ext.Iceoryx2ServiceVariant
    Iceoryx2Mode = _ext.Iceoryx2Mode
    __all__.extend(["iceoryx2_sub", "Iceoryx2ServiceVariant", "Iceoryx2Mode"])

# User-friendly aliases for FIX functions
fix_connect = _ext.py_fix_connect
fix_connect_tls = _ext.py_fix_connect_tls
fix_accept = _ext.py_fix_accept

# User-friendly aliases for OTLP
# (otlp_push is exposed as a stream method, no top-level sub function needed)

# User-friendly aliases for Prometheus
PrometheusExporter = _ext.PrometheusExporter
__all__.append("PrometheusExporter")

# User-friendly aliases for the web adapter
WebServer = _ext.WebServer
__all__.append("WebServer")
