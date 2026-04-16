use log::Level;
use pyo3::BoundObject;
use std::any::type_name;

use ::wingfoil::{Element, IntoStream, NodeOperators, Stream, StreamOperators};

use pyo3::conversion::IntoPyObject;
use pyo3::prelude::*;

use std::rc::Rc;

use crate::proxy_stream::*;
use crate::py_element::PyElement;
use crate::types::*;
use crate::*;

#[derive(Clone)]
#[pyclass(subclass, unsendable, name = "Stream")]
pub struct PyStream(pub Rc<dyn Stream<PyElement>>);

impl PyStream {
    fn extract<T>(&self) -> Rc<dyn Stream<T>>
    where
        T: Element + for<'a, 'py> FromPyObject<'a, 'py>,
    {
        self.0.map(move |x: PyElement| {
            Python::attach(|py| match x.as_ref().extract::<T>(py) {
                Ok(val) => val,
                Err(_err) => {
                    panic!("Failed to convert from python type to native rust type")
                }
            })
        })
    }

    pub fn inner_stream(&self) -> Rc<dyn Stream<PyElement>> {
        self.0.clone()
    }

    pub fn from_inner(inner: Rc<dyn Stream<PyElement>>) -> Self {
        Self(inner)
    }
}

pub fn to_pyany<T>(x: T) -> Py<PyAny>
where
    T: for<'py> IntoPyObject<'py>,
{
    Python::attach(|py| match x.into_pyobject(py) {
        Ok(bound) => bound.into_any().unbind(),
        Err(_) => panic!("Conversion to PyAny from type {} failed", type_name::<T>()),
    })
}

pub fn vec_any_to_pyany(x: Vec<Py<PyAny>>) -> Py<PyAny> {
    Python::attach(|py| x.into_pyobject(py).unwrap().into_any().unbind())
}

pub trait AsPyStream<T>
where
    T: Element + for<'py> IntoPyObject<'py>,
{
    fn as_py_stream(&self) -> PyStream;
}

impl<T> AsPyStream<T> for Rc<dyn Stream<T>>
where
    T: Element + for<'py> IntoPyObject<'py>,
{
    fn as_py_stream(&self) -> PyStream {
        let strm = self.map(|x| {
            let py_any = to_pyany(x);
            PyElement::new(py_any)
        });
        PyStream(strm)
    }
}

#[pymethods]
impl PyStream {
    #[new]
    fn new(inner: Py<PyAny>) -> Self {
        let stream = PyProxyStream::new(inner);
        let stream = stream.into_stream();
        Self(stream)
    }

    #[pyo3(signature = (realtime, start=None, duration=None, cycles=None))]
    fn run(
        &self,
        py: Python<'_>,
        realtime: bool,
        start: Option<Py<PyAny>>,
        duration: Option<Py<PyAny>>,
        cycles: Option<u32>,
    ) -> PyResult<()> {
        let (run_mode, run_for) =
            parse_run_args(py, realtime, start, duration, cycles).to_pyresult()?;

        // Convert fat pointer to (addr, vtable) pair which is Send+Sync
        let stream_ptr = Rc::as_ptr(&self.0);
        let (addr, vtable): (usize, usize) = unsafe { std::mem::transmute(stream_ptr) };

        // Release GIL during the run to allow async tasks to acquire it
        // SAFETY: The Rc is kept alive by self for the duration of this call
        let result = py.detach(move || {
            // Reconstruct the fat pointer from (addr, vtable)
            let stream_ptr: *const dyn Stream<PyElement> =
                unsafe { std::mem::transmute((addr, vtable)) };
            // Temporarily reconstruct the Rc without taking ownership
            let stream = unsafe { Rc::from_raw(stream_ptr) };
            let result = stream.run(run_mode, run_for);
            std::mem::forget(stream); // Don't drop the Rc (self.0 still owns it)
            result
        });
        result.to_pyresult()?;
        Ok(())
    }

    fn peek_value(&self) -> Py<PyAny> {
        self.0.peek_value().value()
    }

    // begin StreamOperators

    fn collect(&self) -> PyStream {
        let strm = self.0.collect().map(|items| {
            Python::attach(move |py| {
                let items = items
                    .iter()
                    .map(|item| item.value.as_ref().clone_ref(py))
                    .collect::<Vec<_>>();
                PyElement::new(vec_any_to_pyany(items))
            })
        });
        PyStream(strm)
    }

    fn dataframe(&self) -> PyStream {
        let time_stream = self.0.clone().as_node().ticked_at_elapsed();

        let zipped = ::wingfoil::bimap(
            Dep::Active(self.0.clone()),
            Dep::Active(time_stream),
            |val: PyElement, time: ::wingfoil::NanoTime| {
                Python::attach(|py| {
                    let time_secs: f64 = time.into();

                    let py_tuple = pyo3::types::PyTuple::new(
                        py,
                        &[
                            time_secs.into_pyobject(py).unwrap().into_any(),
                            val.value().into_bound(py),
                        ],
                    )
                    .unwrap();

                    PyElement::new(py_tuple.into_any().unbind())
                })
            },
        );

        let strm = zipped.collect().map(|items| {
            Python::attach(move |py| {
                let items = items
                    .iter()
                    .map(|item| item.value.as_ref().clone_ref(py))
                    .collect::<Vec<_>>();
                PyElement::new(vec_any_to_pyany(items))
            })
        });

        PyStream(strm)
    }

    fn average(&self) -> PyStream {
        self.extract::<f64>().average().as_py_stream()
    }

    fn buffer(&self, capacity: usize) -> PyStream {
        let strm = self.0.buffer(capacity).map(|items| {
            Python::attach(move |py| {
                let items = items
                    .iter()
                    .map(|item| item.as_ref().clone_ref(py))
                    .collect::<Vec<_>>();
                PyElement::new(vec_any_to_pyany(items))
            })
        });
        PyStream(strm)
    }

    fn finally(&self, func: Py<PyAny>) -> PyNode {
        let node = self.0.finally(|py_elmnt, _| {
            Python::attach(move |py| {
                let res = py_elmnt.as_ref().clone_ref(py);
                let args = (res,);
                func.call1(py, args).unwrap();
            });
            Ok(())
        });
        PyNode(node)
    }

    fn for_each(&self, func: Py<PyAny>) -> PyNode {
        let node = self.0.for_each(move |py_elmnt, t| {
            Python::attach(|py| {
                let res = py_elmnt.as_ref().clone_ref(py);
                let t: f64 = t.into();
                let args = (res, t);
                func.call1(py, args).unwrap();
            });
        });
        PyNode(node)
    }

    fn inspect(&self, func: Py<PyAny>) -> PyStream {
        let stream = self.0.inspect(move |x| {
            Python::attach(|py| {
                func.call1(py, (x.value(),)).unwrap();
            });
        });
        PyStream(stream)
    }

    /// difference in its source from one cycle to the next (pass-through of PyElement)
    fn difference(&self) -> PyStream {
        PyStream(self.0.difference())
    }

    /// Propagates its source delayed by specified duration (milliseconds)
    fn delay(&self, delay_secs: f64) -> PyStream {
        let delay = Duration::from_secs_f64(delay_secs);
        PyStream(self.0.delay(delay))
    }

    /// only propagates its source if it changed (uses PartialEq on PyElement)
    fn distinct(&self) -> PyStream {
        PyStream(self.0.distinct())
    }

    /// drops source contingent on supplied predicate (Python callable)
    fn filter(&self, keep_func: Py<PyAny>) -> PyStream {
        let keep = self.0.map(move |x| {
            Python::attach(|py| {
                keep_func
                    .call1(py, (x.value(),))
                    .unwrap()
                    .extract::<bool>(py)
                    .unwrap()
            })
        });
        PyStream(self.0.filter(keep))
    }

    /// propagates source up to limit times
    fn limit(&self, limit: u32) -> PyStream {
        PyStream(self.0.limit(limit))
    }

    /// logs source and propagates it. Default level INFO.
    fn logged(&self, label: String) -> PyStream {
        PyStream(self.0.logged(&label, Level::Info))
    }

    /// Map’s its source into a new Stream using the supplied Python callable.
    fn map(&self, func: Py<PyAny>) -> PyStream {
        let stream = self.0.map(move |x| {
            Python::attach(|py| {
                let res = func.call1(py, (x.value(),)).unwrap();
                PyElement::new(res)
            })
        });
        PyStream(stream)
    }

    // /// negates its input (for boolean-like PyElements)
    fn not(&self) -> PyStream {
        PyStream(self.0.not())
    }

    fn sample(&self, trigger: Py<PyAny>) -> PyStream {
        Python::attach(|py| {
            let obj = trigger.as_ref();
            if let Ok(node) = obj.extract::<PyRef<PyNode>>(py) {
                return PyStream(self.0.sample(node.0.clone()));
            }
            if let Ok(stream) = obj.extract::<PyRef<PyStream>>(py) {
                return PyStream(self.0.sample(stream.0.clone()));
            }
            panic!("Expected a PyNode or PyStream");
        })
    }

    /// sum the stream (extracts f64 values before summing)
    fn sum(&self) -> PyStream {
        self.extract::<f64>().sum().as_py_stream()
    }

    fn count(&self) -> PyStream {
        self.0.count().as_py_stream()
    }

    /// Pairs each value with the graph time as a `(float, value)` tuple,
    /// where the float is seconds since Unix epoch.
    fn with_time(&self) -> PyStream {
        let strm = self.0.with_time().map(|(t, v)| {
            Python::attach(|py| {
                let time_secs: f64 = t.into();
                let py_tuple = pyo3::types::PyTuple::new(
                    py,
                    &[
                        time_secs.into_pyobject(py).unwrap().into_any(),
                        v.value().into_bound(py),
                    ],
                )
                .unwrap();
                PyElement::new(py_tuple.into_any().unbind())
            })
        });
        PyStream(strm)
    }

    /// Write this stream of dicts to a CSV file.
    ///
    /// Each dict becomes one CSV row. Headers are inferred from the first dict's
    /// keys, and a `time` column is prepended with the graph time in nanoseconds.
    ///
    /// Args:
    ///     path: Output file path
    ///
    /// Returns:
    ///     A Node that drives the write operation.
    fn csv_write(&self, path: String) -> PyNode {
        PyNode::new(crate::py_csv::py_csv_write_inner(&self.0, path))
    }

    /// Write this stream to a KDB+ table.
    ///
    /// Args:
    ///     host: KDB+ server hostname
    ///     port: KDB+ server port
    ///     table: Name of the target KDB+ table
    ///     columns: List of (name, type) tuples for non-time columns.
    ///              Supported types: "symbol", "float", "long", "int", "bool"
    ///
    /// Returns:
    ///     A Node that drives the write operation.
    #[pyo3(signature = (host, port, table, columns))]
    fn kdb_write(
        &self,
        host: String,
        port: u16,
        table: String,
        columns: Vec<(String, String)>,
    ) -> PyResult<PyNode> {
        let conn = ::wingfoil::adapters::kdb::KdbConnection::new(host, port);
        let node = crate::py_kdb::py_kdb_write_inner(conn, table, columns, &self.0)?;
        Ok(PyNode::new(node))
    }

    /// Publish this stream of dicts to etcd via PUT.
    ///
    /// Stream values must be dicts with `"key"` (str) and `"value"` (bytes),
    /// or lists of such dicts for multiple writes per tick.
    ///
    /// Args:
    ///     endpoint: etcd endpoint, e.g. `"http://localhost:2379"`
    ///     lease_ttl: optional lease TTL in seconds; keys expire after this duration
    ///                and vanish immediately on clean shutdown. Pass `None` for
    ///                persistent keys (default).
    ///     force: if `True` (default), silently overwrite existing keys.
    ///            If `False`, fail if any key already exists.
    ///
    /// Returns:
    ///     A Node that drives the write operation.
    #[cfg(feature = "etcd")]
    #[pyo3(signature = (endpoint, lease_ttl=None, force=true))]
    fn etcd_pub(&self, endpoint: String, lease_ttl: Option<f64>, force: bool) -> PyNode {
        PyNode::new(crate::py_etcd::py_etcd_pub_inner(
            &self.0, endpoint, lease_ttl, force,
        ))
    }

    /// Publish this stream of bytes to a ZMQ PUB socket bound on the given port.
    ///
    /// The stream values must be `bytes` objects. Only supported in real-time mode.
    ///
    /// Args:
    ///     port: TCP port to bind the PUB socket on
    ///
    /// Returns:
    ///     A Node that drives the publish operation.
    fn zmq_pub(&self, port: u16) -> PyNode {
        PyNode::new(crate::py_zmq::py_zmq_pub_inner(&self.0, port))
    }

    // ── Latency stamping ─────────────────────────────────────────────────

    /// Stamp a named latency stage on each tick using the cycle-start
    /// wall-clock time. The stream must carry `TracedBytes` values.
    ///
    /// Args:
    ///     stage: Stage name (must match one of the names in the `Latency`).
    ///
    /// Returns:
    ///     A new Stream with the stage stamped.
    fn stamp(&self, stage: String) -> PyStream {
        PyStream(crate::py_latency::py_stamp_inner(&self.0, stage, false))
    }

    /// Like `stamp`, but only inserts the stamp node when `enabled` is True.
    /// When False, returns the stream unchanged — zero runtime cost.
    #[pyo3(signature = (stage, enabled))]
    fn stamp_if(&self, stage: String, enabled: bool) -> PyStream {
        if enabled {
            self.stamp(stage)
        } else {
            self.clone()
        }
    }

    /// Stamp a named latency stage with a precise wall-clock read (~5-10ns
    /// TSC read per tick). Gives intra-cycle resolution.
    fn stamp_precise(&self, stage: String) -> PyStream {
        PyStream(crate::py_latency::py_stamp_inner(&self.0, stage, true))
    }

    /// Like `stamp_precise`, but only inserts the stamp node when `enabled`
    /// is True. When False, returns the stream unchanged.
    #[pyo3(signature = (stage, enabled))]
    fn stamp_precise_if(&self, stage: String, enabled: bool) -> PyStream {
        if enabled {
            self.stamp_precise(stage)
        } else {
            self.clone()
        }
    }

    /// Install a latency report sink. The stream must carry `TracedBytes`
    /// values. Per-stage delta statistics (count/min/mean/p50/p99/max) are
    /// printed on graph shutdown.
    ///
    /// Args:
    ///     stages: Stage names in order (same list used for `Latency`).
    ///     print_on_teardown: Whether to print the report on shutdown (default True).
    ///
    /// Returns:
    ///     A Node that drives the report sink.
    #[pyo3(signature = (stages, print_on_teardown=true))]
    fn latency_report(&self, stages: Vec<String>, print_on_teardown: bool) -> PyNode {
        PyNode::new(crate::py_latency::py_latency_report_inner(
            &self.0,
            stages,
            print_on_teardown,
        ))
    }

    /// Like `latency_report`, but only installs the sink when `enabled` is
    /// True. When False, returns the upstream as a Node (no report sink).
    #[pyo3(signature = (stages, enabled, print_on_teardown=true))]
    fn latency_report_if(
        &self,
        stages: Vec<String>,
        enabled: bool,
        print_on_teardown: bool,
    ) -> PyNode {
        if enabled {
            self.latency_report(stages, print_on_teardown)
        } else {
            PyNode::new(self.0.clone().as_node())
        }
    }

    /// Publish this stream of bytes to an iceoryx2 service.
    ///
    /// When `stages` is provided, expects the stream to carry `TracedBytes`
    /// values and serializes as `latency_header + payload_bytes` on the wire.
    ///
    /// Args:
    ///     service_name: iceoryx2 service name, e.g. `"my/service"`
    ///     variant: Service variant ("ipc" or "local")
    ///     history_size: Service history ring size (must match subscribers)
    ///     initial_max_slice_len: Initial maximum slice length (bytes)
    ///     stages: Optional list of latency stage names for instrumented mode
    ///
    /// Returns:
    ///     A Node that drives the publish operation.
    #[cfg(feature = "iceoryx2-beta")]
    #[pyo3(signature = (service_name, variant=crate::py_iceoryx2::PyIceoryx2ServiceVariant::Ipc, history_size=5, initial_max_slice_len=128*1024, stages=None))]
    fn iceoryx2_pub(
        &self,
        service_name: String,
        variant: crate::py_iceoryx2::PyIceoryx2ServiceVariant,
        history_size: usize,
        initial_max_slice_len: usize,
        stages: Option<Vec<String>>,
    ) -> PyNode {
        PyNode::new(crate::py_iceoryx2::py_iceoryx2_pub_inner(
            &self.0,
            service_name,
            variant,
            history_size,
            initial_max_slice_len,
            stages,
        ))
    }

    /// Push this stream as an OTLP gauge metric.
    ///
    /// Args:
    ///     metric_name: Name of the metric to report
    ///     endpoint: OTLP HTTP endpoint, e.g. `"http://localhost:4318"`
    ///     service_name: Service name reported in OTLP resource attributes
    ///
    /// Returns:
    ///     A Node that drives the push operation.
    fn otlp_push(&self, metric_name: String, endpoint: String, service_name: String) -> PyNode {
        crate::py_otlp::py_otlp_push_inner(self, metric_name, endpoint, service_name)
    }

    /// Publish this stream of bytes and register as `name` in etcd.
    ///
    /// Binds on `127.0.0.1`; use `zmq_pub_etcd_on` for multi-host deployments
    /// where `127.0.0.1` is not routable by subscribers on other hosts.
    ///
    /// Args:
    ///     name: Name / etcd key to register under (e.g. "quotes")
    ///     port: TCP port to bind the PUB socket on
    ///     endpoint: etcd endpoint (e.g. "http://localhost:2379")
    ///
    /// Returns:
    ///     A Node that drives the publish operation.
    #[cfg(feature = "etcd")]
    fn zmq_pub_etcd(&self, name: String, port: u16, endpoint: String) -> PyNode {
        PyNode::new(crate::py_zmq::py_zmq_pub_etcd_inner(
            &self.0, name, port, endpoint,
        ))
    }

    /// Like `zmq_pub_etcd` but binds on `address` instead of `127.0.0.1`.
    ///
    /// Args:
    ///     name: Name / etcd key to register under
    ///     address: Routable bind address (e.g. "192.168.1.10")
    ///     port: TCP port to bind the PUB socket on
    ///     endpoint: etcd endpoint
    ///
    /// Returns:
    ///     A Node that drives the publish operation.
    #[cfg(feature = "etcd")]
    fn zmq_pub_etcd_on(
        &self,
        name: String,
        address: String,
        port: u16,
        endpoint: String,
    ) -> PyNode {
        PyNode::new(crate::py_zmq::py_zmq_pub_etcd_on_inner(
            &self.0, name, address, port, endpoint,
        ))
    }

    // end StreamOperators
}
