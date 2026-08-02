.. _wingfoil-next-api:

API Reference
=============

This page documents the public API exposed by the top-level ``wingfoil_next``
module. See the :doc:`User Guide <readme>` for narrative examples, and
:doc:`migration` if you are moving from the legacy ``wingfoil`` package.

.. Deliberately no `.. currentmodule::` here. With one in force, the
   autosummary at the foot of this page resolves its bare module name relative
   to it (`wingfoil_next.wingfoil_next`) and fails to import; targets are
   therefore written out in full instead.

.. note::

   Every I/O adapter binding sits behind a **cargo feature of the same name**,
   so the functions actually present depend on how the extension was built. The
   published wheel carries all of them except ``aeron`` and ``iceoryx2``, which
   are opt-in (``maturin develop -F aeron``) because they would make the wheel
   platform-specific. A missing adapter is simply an absent module attribute —
   ``hasattr(wingfoil_next, "kafka_sub")`` is the check.

Quick index
-----------

**Core types**: :class:`~wingfoil_next.Graph`, :class:`~wingfoil_next.Stream`,
:class:`~wingfoil_next.CustomStream`.

**Sources (methods on** :class:`~wingfoil_next.Graph` **)**:

``constant(value)``, ``counter(period_nanos)``, ``values(values,
period_nanos)``, ``custom_node(upstreams, obj)``; plus ``run(cycles=None,
duration_nanos=None, realtime=False, start_nanos=0)``.

**Stream combinators (methods on** :class:`~wingfoil_next.Stream` **)**:

*Transform* — ``map``, ``filter_map``, ``fold``, ``reduce``, ``bimap``,
``difference``, ``not`` (a Python keyword — reach it with
``getattr(stream, "not")()``), ``split``.

*Gate* — ``filter`` (gates on another *stream*'s current value), ``filter_value``
(gates on a *predicate* — this is legacy's ``filter``), ``filter_none``,
``distinct``, ``drop_small_change``, ``limit``, ``throttle``, ``sample``,
``delay``.

*Combine* — ``merge``, ``merge_all``.

*Aggregate* — ``count``, ``sum``, ``mean`` (alias ``average``), ``accumulate``,
``buffer``, ``window``, ``collect``, ``with_time``.

*Observe* — ``inspect``, ``print``, ``logged``, ``dataframe``, ``value``.

**Statistics**: cumulative ``sum`` / ``mean`` are on
:class:`~wingfoil_next.Stream`. The windowed moment surface (``variance``,
``std``, ``median``, rolling windows, ``ewma``) lives in the Rust
``wingfoil_next::stats`` layer and reaches Python through the plugin seam
rather than as fixed :class:`~wingfoil_next.Stream` methods — see
`Plugin seam`_.

**Rust-authored demo components** (the plugin seam, proven end to end):
``scale``, ``square``, ``running_total``, ``weighted_add``, ``blend3``,
``blend4``, ``clamped_scale``, ``doubled_running_total``, ``spread_and_mid``,
``ramp_source``, ``pair_source``, ``split_source``, ``list_sink``,
``burst_list_sink``, ``compiled_island``, ``interpreted_twin``.

**Latency tracing** (always present, never feature-gated): ``Latency``,
``TracedBytes``, ``LatencyStats``, ``stamp``, ``stamp_if``, ``stamp_precise``,
``stamp_precise_if``, ``latency_report``, ``latency_report_if``.

**I/O adapters** — see `I/O adapters`_ below.

Core types
----------

.. The classes below are also documented in full on the generated module page
   (see `Generated reference`_), which is where the index entries live; these
   inline copies carry `:no-index:` so the two do not collide.

.. autoclass:: wingfoil_next.Graph
   :members:
   :undoc-members:
   :no-index:

.. autoclass:: wingfoil_next.Stream
   :members:
   :undoc-members:
   :no-index:

Python-defined nodes
--------------------

A Python object can *be* a graph node. There are two forms over the same
machinery: composition via ``Graph.custom_node(upstreams, obj)``, and
inheritance via :class:`~wingfoil_next.CustomStream`.

.. autoclass:: wingfoil_next.CustomStream
   :members:
   :undoc-members:
   :no-index:

.. autoclass:: wingfoil_next.stream.UpstreamValue
   :members:

.. warning::

   A graph containing a Python-defined node is **single-run**: the instance's
   state is caller-owned and the engine has no hook to reset it, so a second
   ``run()`` raises rather than replaying from dirty state.

Bursts: why a source tick is a ``list``
---------------------------------------

Every I/O source in next is **burst-shaped**. A burst is the group of values
that share one instant — the messages that arrived between two graph cycles, or
the rows in a replay that carry the same timestamp. The engine never collapses
a burst to "latest wins" and never drops a member, so the Python edge erases a
burst to a **``list``**: one tick, one list, in arrival order.

That is why ``postgres_read``, ``csv_read``, ``kdb_read``, ``kafka_sub``,
``redis_sub``, ``etcd_sub``, ``fluvio_sub``, ``zmq_sub``, ``fix_connect`` and
the rest all yield a list per tick even when the list usually has one element.
For the single-value case, index it::

   rows = wingfoil_next.csv_read(g, "prices.csv", "time")
   first = rows.map(lambda batch: batch[0])

The legacy bindings collapsed these bursts and kept only the last row of any
timestamp, which silently dropped the rest. See :doc:`migration`.

Run modes and the run window
----------------------------

:meth:`Graph.run <wingfoil_next.Graph.run>` takes the mode and the bound
together:

.. code-block:: python

   g.run(cycles=3)                          # historical replay from t=0
   g.run(start_nanos=T0, duration_nanos=D)  # historical, a fixed window
   g.run(realtime=True, duration_nanos=D)   # wall clock
   g.run(realtime=True)                     # forever

A Rust caller's adapter learns the run mode from the graph; a Python
:class:`~wingfoil_next.Graph` does not know its mode until ``run()`` is called, and
several adapters need it at *wiring* time — a live subscriber has no timeline
to replay, and a sliced historical read must know its window to build its
queries. Those adapters therefore take the fact as an **argument**
(``realtime=``, or ``start_nanos=`` / ``duration_nanos=``) and it must match the
eventual ``run(...)``. Passing ``realtime=False`` to a live-only source raises
at wiring rather than producing an empty run.

Latency tracing
---------------

Stamp wall-clock timestamps onto messages as they hop through a graph (and
across processes), then aggregate the per-stage deltas at the end of the
pipeline:

.. code-block:: python

   stages = ["ingest", "decode", "publish"]
   messages = source.map(lambda p: wf.TracedBytes(p, wf.Latency(stages)))

   stamped = wf.stamp(wf.stamp(wf.stamp(messages, "ingest"), "decode"), "publish")
   sink, stats = wf.latency_report(stamped, stages, print_on_teardown=False)

   g.run(cycles=1000)
   print(stats["decode"]["p99_ns"], stats.report())

* ``stamp`` reads the cycle-start clock; ``stamp_precise`` takes a fresh clock
  read per tick, for intra-cycle resolution.
* Every entry point has an ``_if(..., enabled)`` variant that wires nothing when
  disabled — and still returns the same *shape*, so call sites do not branch.
* ``Latency.to_bytes()`` / ``Latency.from_bytes(data, stages)`` are the
  little-endian header a Rust peer reads straight back as its
  ``latency_stages!`` record.
* Aggregation and report formatting are the engine's own, so a Python report is
  byte-identical to a Rust one.
* Bursts are stamped **element-wise**: a value reaching ``stamp`` may be a list
  of ``TracedBytes``, and every member is stamped under one GIL attach.

.. important::

   ``latency_report`` returns a **tuple** ``(sink, LatencyStats)``. The legacy
   binding returned only the sink and could only *print* the numbers; the
   engine hands back a shared stats handle, so the second element lets you read
   them after the run. ``latency_report_if(..., enabled=False)`` returns a sink
   that never ticks plus an all-zero stats handle — legacy returned the
   *upstream* node, changing the call's return type. This is deviation **D13**
   in ``docs/deviation-register.md``.

.. autoclass:: wingfoil_next.Latency
   :members:
   :undoc-members:
   :no-index:

.. autoclass:: wingfoil_next.TracedBytes
   :members:
   :undoc-members:
   :no-index:

.. autoclass:: wingfoil_next.LatencyStats
   :members:
   :undoc-members:
   :no-index:

I/O adapters
------------

All fifteen engine adapters are bound. Each is a module-level function (or, for
the stateful ones, a class) rather than a :class:`Stream` method, so a binding
authored in a third-party crate looks identical to a built-in one.

.. list-table::
   :header-rows: 1
   :widths: 14 40 46

   * - Adapter
     - Entry points
     - Notes
   * - **PostgreSQL**
     - ``postgres_read(graph, …)``, ``postgres_sub(graph, …)``,
       ``postgres_source(graph, …)``, ``postgres_write(stream, …)``,
       ``postgres_notify_trigger_sql()``
     - Rows are ``dict``\ s; a tick is a ``list`` of rows. ``postgres_read`` is
       a sliced historical replay (needs ``start_nanos`` / ``duration_nanos``);
       ``postgres_sub`` is a live ``LISTEN``/``NOTIFY`` tail; ``postgres_source``
       dispatches on the run mode. Writes declare ``columns`` as
       ``(name, sql_type)`` pairs in table order.
   * - **Kafka**
     - ``kafka_sub(graph, …)``, ``kafka_pub(stream, …)``
     - Events are ``{topic, partition, offset, key, value}`` dicts. ``topic`` is
       *optional* on the sink — a record naming its own target lets one sink
       write to many topics.
   * - **Redis**
     - ``redis_sub``, ``redis_pub``, ``redis_stream_read``,
       ``redis_stream_write``
     - Pub/Sub events erase to ``{channel, payload}``; stream events to
       ``{key, id, fields}``. ``channel`` / ``key`` are optional fallbacks on
       the sinks. Both sinks take ``buffer_size`` back-pressure.
   * - **etcd**
     - ``etcd_sub(graph, …)``, ``etcd_pub(stream, …)``
     - Events are ``{kind, key, value, revision}`` with ``kind`` a string
       (``"put"`` / ``"delete"``). ``endpoints`` accepts a single ``str`` or a
       list, exposing cluster support.
   * - **Fluvio**
     - ``fluvio_sub(graph, …)``, ``fluvio_pub(stream, …)``
     - Events are ``{key, value, offset}``. The key is **asymmetric** — a read
       yields ``bytes | None``, a write expects ``str | None`` — because that is
       the adapter's own shape; round-tripping needs an explicit ``.decode()``.
   * - **CSV**
     - ``csv_read(graph, …)``, ``csv_write(stream, …)``
     - Deterministic historical replay. Values are ``str`` on both sides (CSV
       has no types). Column order follows the file header. ``buffer_size``
       bounds the replay look-ahead so a huge file is not read up front.
   * - **KDB+**
     - ``kdb_read(graph, …)``, ``kdb_sub(graph, …)``, ``kdb_write(stream, …)``
     - Rows are ``dict``\ s dispatched on each value's actual KDB type. Temporal
       columns decode to their **raw ``int``** (nanoseconds from the KDB epoch,
       2000-01-01) rather than a ``datetime`` that would have to guess a
       timezone. ``kdb_sub`` (the tickerplant tail) is new against legacy.
   * - **ZeroMQ**
     - ``zmq_sub``, ``zmq_sub_etcd``, ``zmq_pub``, ``zmq_pub_etcd``
     - ``zmq_sub`` returns a **tuple** ``(data, status)``; ``status`` ticks only
       on a transition, carrying ``"connected"`` / ``"disconnected"``. Payloads
       cross as ``bytes``. The ``_etcd`` pair needs the ``etcd`` feature too.
   * - **FIX 4.4**
     - ``fix_connect``, ``fix_accept``, ``fix_send``, ``fix_connect_tls``,
       ``FixConnection``
     - Messages are ``{"msg_type": str, "seq_num": int, "fields": [(tag, value)]}``
       with **``str`` tag values** (FIX is a text protocol — spell a number
       ``str(price)``). Session status is *always* a dict. ``fix_connect_tls``
       hands back a ``FixConnection`` handle.
   * - **augurs**
     - ``augurs_forecast``, ``augurs_changepoint``, ``augurs_seasons``,
       ``augurs_outlier``, ``augurs_dtw``, ``augurs_cluster``
     - The first three analyse **one** series (a stream of floats); the last
       three compare **several** (a stream of lists of floats). Results are
       ``dict``\ s carrying the full model output, not just the headline number.
       ``model`` / ``detector`` / ``metric`` are strings.
   * - **Prometheus**
     - ``PrometheusExporter(addr)``, ``.serve() -> port``,
       ``.gauge(name, stream)``
     - A stateful handle. Historical runs are a **no-op** — a backtest never
       publishes fast-forwarded values to a live scrape endpoint.
   * - **OTLP**
     - ``otlp_push(stream, …)``
     - Pushes a gauge metric per tick, stringifying via Python's ``str()``.
       Historical runs are a no-op, as above.
   * - **web**
     - ``WebServer(addr, …)``, ``.port()``, ``.codec_name()``,
       ``.sub(graph, topic)``, ``.pub(stream, topic)``,
       ``.pub_bursts(stream, topic)``, ``.stop()``
     - A stateful handle; publishing is a **server** method, not a stream
       method. Values marshal through JSON; ``bytes`` become an array of ints
       (wire-compatible with a Rust ``Vec<u8>`` peer, and therefore decoded back
       as a ``list``, not ``bytes``). ``pub_bursts`` puts a whole same-instant
       group on the wire as one atomic frame.
   * - **Aeron**
     - ``aeron_sub``, ``aeron_sub_with_status``, ``aeron_pub``,
       ``aeron_pub_with_status``
     - **Not in the default wheel** — ``rusteron-client`` builds the Aeron C
       library from source (clang, libuuid, CMake ≥ 3.30). Connecting is
       *eager*: an unreachable media driver raises at wiring. ``mode`` is a
       string (``"spin"`` / ``"threaded"``).
   * - **iceoryx2**
     - ``iceoryx2_sub(graph, …)``, ``iceoryx2_pub(stream, …)``
     - **Not in the default wheel** — Linux/POSIX-only. Uses the slice
       (``[u8]``) API; payloads cross as ``bytes``. ``variant`` and ``mode`` are
       strings. Both entry points take an optional ``stages`` list: with it a
       sample is a ``[u64; len(stages)]`` little-endian stamp header followed by
       the payload, and the value is a :class:`~wingfoil_next.TracedBytes`
       rather than ``bytes`` — the same layout a Rust peer's ``latency_stages!``
       record has, so a Python subscriber reads a Rust publisher's stamps.

Two conventions run through all of them:

**Selectors are strings, not enum classes.** A poll mode, a service variant, an
etcd event kind, an augurs model — each is a plain ``str``, and a wrong value
raises listing the accepted set. This keeps the module surface small and means
a third-party adapter binding needs no class registration to look native.

**Conversion fails loudly.** A missing required field, a wrong-typed value, or
a non-``dict`` where a record was expected **aborts the run** naming the
offending field. The legacy bindings defaulted their way through these — a
missing payload became empty bytes, a bad ``__str__`` became an empty metric
value, a non-dict became an empty burst that published nothing at all. Those
silent-data-loss paths are gone; see :doc:`migration`.

.. _plugin-seam:

Plugin seam
-----------

The point of the binding is not just to expose the built-ins: you can author an
op, a whole sub-graph, or an I/O adapter **in Rust** and call it from Python
alongside the built-in vocabulary. Three macros do it, and a binding written in
a third-party crate is indistinguishable from a built-in one:

``#[pyop]`` / ``pyop_fn!``
   Expose an ``Op`` implementation as ``module.my_op(stream, …)``. Handles one
   to four inputs, tuple ``Cfg`` (each element becomes its own named Python
   parameter), and stateful ops.

``#[pygraph]``
   Expose a Rust wiring function (``fn(&Stream<T>) -> Stream<U>``) as one Python
   callable that splices its nodes into the caller's graph. Multi-input and
   tuple-returning (multi-output) forms are supported.

``#[pyadapter]``
   Expose a source or sink adapter trait implemented on ``GraphBuilder`` or
   ``Stream<T>``. Tuple returns give the ``(data, status)`` shape live sources
   use.

The interior of any of these stays **natively typed** — only the Python-facing
edge erases to the boxed ``PyElement``. Combined with ``compiled_island``,
that gives "compiled interiors, dynamic wiring": Python composes the graph at
run time while the hot sub-graphs run as monomorphized straight-line code.

.. code-block:: python

   import wingfoil_next as wf

   g = wf.Graph()
   ramp = wf.ramp_source(g, 10.0, 2.0)     # a Rust source adapter
   squared = wf.square(ramp)               # a Rust op
   totals = wf.doubled_running_total(ramp) # a Rust sub-graph

   collected = []
   wf.list_sink(squared, collected)        # a Rust sink adapter
   g.run(cycles=3)

The functions above are the demo components this crate ships to prove the seam
end to end; see ``docs/python-interop.md`` for the design and
``examples/plugin_sdk.py`` for the runnable version.

Generated reference
-------------------

The table below is auto-generated by ``sphinx-autosummary`` and links to
per-member pages with full signatures and docstrings. It reflects the features
the extension was **built with**, so a build without an adapter feature simply
omits it.

.. autosummary::
   :toctree: api_generated

   wingfoil_next
