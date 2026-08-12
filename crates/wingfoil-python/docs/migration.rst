.. _wingfoil-migration:

Migrating from ``wingfoil``
===========================

``wingfoil`` **supersedes** the legacy ``wingfoil`` Python package. It is a
replacement engine with its own binding, not a compatibility facade over the old
one, and that is a deliberate breaking change: the import name changes, some
entry points are renamed, and a number of behaviours that used to fail silently
now raise.

This page is the complete list of what moves. Everything the legacy package
could do, ``wingfoil`` can do; where the shape differs, the reason is given
rather than papered over.

.. contents::
   :local:
   :depth: 1

The import
----------

.. code-block:: diff

   - import wingfoil as wf
   + import wingfoil as wf

The package is ``wingfoil-python`` on the index and ``wingfoil`` on
``import``. The compiled extension underneath is the private
``wingfoil._wingfoil``; the ``wingfoil`` package re-exports it
wholesale, so you never name it.

The graph is explicit
---------------------

Legacy wingfoil had an ambient graph: sources were free functions
(``ticker(seconds)``, ``constant(value)``) that built nodes out of thin air, and
``run`` was a method on whatever stream or node you happened to be holding.
Legacy's ``Graph(nodes)`` was a different thing — a *bag of roots* you passed to
``run`` when one terminal node was not enough to reach them all.

Wingfoil has no ambient graph. You hold a :class:`~wingfoil.Graph` — an open
builder — and build every source **on** it:

.. code-block:: diff

   - stream = wf.ticker(0.000001).map(f)
   - stream.run(realtime=False, cycles=100)
   + g = wf.Graph()
   + stream = g.counter(period_nanos=1_000).map(f)
   + g.run(cycles=100)

``run`` is a **graph** method, not a stream/node method, and it takes nanosecond
integers rather than seconds-as-float and ``datetime`` objects. It also defaults
to historical replay, where legacy made ``realtime`` a required positional:

.. code-block:: python

   g.run(cycles=3)                          # historical replay from t=0
   g.run(start_nanos=T0, duration_nanos=D)  # historical, a fixed window
   g.run(realtime=True, duration_nanos=D)   # wall clock
   g.run(realtime=True)                     # forever

Read a value back with ``stream.value()`` (legacy: ``peek_value()``).

Sources
-------

.. list-table::
   :header-rows: 1
   :widths: 40 60

   * - Legacy ``wingfoil``
     - ``wingfoil``
   * - ``ticker(seconds)`` (free function; ticks a bare ``Node``)
     - ``graph.counter(period_nanos=…)`` — ticks the running count ``1, 2, 3, …``,
       so the separate ``.count()`` legacy needed is folded in
   * - ``constant(value)`` (free function)
     - ``graph.constant(value)``
   * - *(no equivalent)*
     - ``graph.values([…], period_nanos=…)`` — replay a finite list, one per
       tick. This is the straightforward way to feed real data in from Python.

Combinators
-----------

The combinator surface is a superset of legacy's. Names that carry over
unchanged: ``map``, ``distinct``, ``difference``, ``delay``, ``limit``,
``sample``, ``count``, ``buffer``, ``collect``, ``with_time``, ``dataframe``,
``inspect``, ``fold``.

.. warning::

   ``filter`` is the one name that carries over with a **different meaning**,
   and it fails loudly rather than silently: wingfoil's
   :meth:`~wingfoil.Stream.filter` gates on another *stream*'s current
   value (matching the Rust engine), so passing legacy's predicate raises
   ``TypeError: 'function' object is not an instance of 'Stream'``. The
   predicate form is :meth:`~wingfoil.Stream.filter_value`.

   .. code-block:: diff

      - odds = counter.filter(lambda n: n % 2 == 1)
      + odds = counter.filter_value(lambda n: n % 2 == 1)

Changed or added:

.. list-table::
   :header-rows: 1
   :widths: 40 60

   * - Legacy ``wingfoil``
     - ``wingfoil``
   * - ``stream.peek_value()``
     - ``stream.value()``
   * - ``stream.filter(predicate)``
     - ``stream.filter_value(predicate)`` — ``filter`` now gates on another
       stream's value (see the warning above)
   * - ``bimap(a, b, f)`` (free function)
     - ``a.bimap(b, f)`` (method)
   * - ``stream.average()``
     - ``stream.mean()`` — ``average()`` is kept as an alias
   * - ``stream.logged(label)``
     - ``stream.logged(label, level="info")`` — the level is now selectable
       (``"trace"``/``"debug"``/``"info"``/``"warn"``/``"error"``)
   * - ``stream.for_each(f)`` / ``stream.finally(f)`` (both return a ``Node``)
     - ``stream.inspect(f)`` for the pass-through tap. Legacy needed the
       ``Node``-returning terminals because ``run`` hung off a node; wingfoil runs
       the graph, so a tap that keeps flowing is the only shape needed.
   * - ``stream.dataframe()`` (a list of ``(time, value)`` tuples)
     - ``stream.collect()`` — the same growing list of pairs, with the time in
       nanoseconds as an int rather than seconds as a float. ``dataframe()`` in
       wingfoil is the upgrade: a real ``pandas.DataFrame`` (columns ``time`` /
       ``value``) assembled in Rust, read back with ``.value()`` after the run
   * - ``to_dataframe`` (free function)
     - ``stream.dataframe()`` — wingfoil builds the frame in the engine, so there is
       no list-to-frame converter to call
   * - ``build_dataframe({name: stream})`` (free function)
     - ``wingfoil.build_dataframe({name: stream})`` — same call, same outer
       join on time. Columns may be held either as frames (``dataframe()``) or
       as ``(time, value)`` tuples (``collect()``, the legacy shape)
   * - *(new)*
     - ``merge``, ``merge_all``, ``throttle``, ``window``, ``accumulate``,
       ``reduce``, ``filter_map``, ``filter_value``, ``filter_none``,
       ``drop_small_change``, ``split``, ``print``

``not`` is still spelled ``not`` (it is a Python keyword, so reach it with
``getattr(stream, "not")()``), and it is arithmetic negation, as in legacy.

Windowed statistics
-------------------

Legacy exposed ``mean`` / ``variance`` / ``std`` / ``sum`` / ``min`` / ``max``
/ ``median`` / ``ewma`` as :class:`~wingfoil.Stream` methods parameterised
by ``Window`` / ``Weighting`` / ``EwmaSpan`` ``#[pyclass]`` objects.

**Nothing to change** — ``wingfoil`` binds the same eight methods with the
same signatures and the same three argument classes, including the ``int`` /
``str`` / ``float`` shorthands and the deliberate rejection of a bare ``float``
window. See :ref:`Statistics <statistics>` for the full surface.

The one thing that moved is *underneath*: wingfoil's engine spells each combination
out as its own statically-checked method (``rolling_mean``,
``time_windowed_mean``, ``cumulative_mean_time_weighted``, …), and the binding
is a dispatcher from the two Python knobs onto them. If you would rather skip
the dispatch and name the engine op directly, the :ref:`plugin seam
<plugin-seam>` lets a Rust ``#[pyop]`` expose exactly the window you want with
no ``Window``/``Weighting`` objects to construct.

Custom nodes
------------

Both legacy forms still work, and there is a new composition form.

.. code-block:: diff

     class MyStream(wf.CustomStream):
         def cycle(self):
             total = sum(u.peek_value() for u in self.upstreams())
             self.set_value(total)
             return True

   - stream = MyStream([a, b])
   + stream = MyStream(graph, [a, b])

Two deviations, both **forced by the engine** rather than chosen:

* **The graph is explicit.** Wingfoil has no ambient graph and a ``Stream`` carries
  no reference back to its builder, so there is nothing to infer it from.
* **``upstreams()`` yields value snapshots, not the upstream ``Stream``
  objects.** During a run the engine holds its runner mutably borrowed, so a
  Python ``cycle`` cannot call back into the graph to read a sibling stream. The
  current values are handed in, wrapped in objects exposing ``peek_value()`` —
  the only stream method legacy ``cycle`` bodies use. A not-yet-ticked upstream
  reads as ``None``.

The new composition form skips the subclass entirely: pass any object with
``cycle(values) -> bool`` and ``peek()`` to ``graph.custom_node(upstreams, obj)``.

A graph containing a Python-defined node is **single-run** — the instance's
state is caller-owned and the engine has no hook to reset it, so a second
``run()`` raises rather than replaying from dirty state.

I/O adapters — the four cross-cutting changes
---------------------------------------------

All fifteen adapters are bound, as they were in legacy — but four changes apply
across every one of them.

1. Sources yield a ``list`` per tick
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

Legacy ``postgres_read`` / ``csv_read`` / ``kdb_read`` collapsed each burst and
kept **only the last row of any timestamp**, silently dropping the rest — the
legacy docstrings warned about it. Wingfoil erases bursts to lists uniformly, so
reads are lossless and every source has the same shape.

.. code-block:: diff

   - rows = wf.csv_read(path, "time")          # a dict per tick
   - price = rows.map(lambda row: row["px"])
   + rows = wf.csv_read(g, path, "time")       # a list of dicts per tick
   + price = rows.map(lambda batch: batch[0]["px"])

2. Conversion fails loudly
~~~~~~~~~~~~~~~~~~~~~~~~~~

The legacy marshaling helpers defaulted their way through every error. A
missing ``value`` produced empty bytes; a wrong-typed ``key`` was dropped; a
non-``dict`` stream value logged an error and produced an **empty burst**,
publishing nothing at all; an object whose ``__str__`` raised became an empty
metric value that parsed as ``0.0`` and went out as real telemetry; a malformed
CSV timestamp silently became ``NanoTime::ZERO``, quietly reordering the replay.

Every one of those now **aborts the run**, naming the offending field. If you
were relying on a default, make it explicit in your ``map``.

3. The run mode / window is an argument
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

A Python :class:`~wingfoil.Graph` does not know its run mode until ``run()`` is called,
but several adapters need it at *wiring* time. Live-only sources take
``realtime=True`` (``realtime=False`` raises there rather than producing an
empty run); sliced historical reads take ``start_nanos`` / ``duration_nanos``.
Both must match the eventual ``graph.run(...)``.

4. Selectors are strings, not enum classes
~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~

Legacy registered ``#[pyclass]`` enums (``Iceoryx2ServiceVariant``,
``Iceoryx2Mode``, ``AeronMode``, …). Wingfoil takes plain strings and raises listing
the accepted set on a wrong value. Fewer classes to import, and a third-party
adapter binding needs no class registration to look native.

.. code-block:: diff

   - wf.iceoryx2_sub(name, wf.Iceoryx2ServiceVariant.Ipc, wf.Iceoryx2Mode.Spin)
   + wf.iceoryx2_sub(g, name, realtime=True, variant="ipc", mode="spin")

Per-adapter renames
-------------------

.. list-table::
   :header-rows: 1
   :widths: 45 55

   * - Legacy ``wingfoil``
     - ``wingfoil``
   * - ``stream.csv_write(path)``
     - ``csv_write(stream, path, columns)`` — the header row is explicit, since
       a dynamic row has no serde field names to derive one from
   * - ``stream.kdb_write(...)``, ``kdb_read(...)``
     - ``kdb_write(stream, …)``, ``kdb_read(graph, …)``; new: ``kdb_sub`` (the
       tickerplant tail) and credentials
   * - ``stream.etcd_pub(endpoint, …)``
     - ``etcd_pub(stream, …)``; ``endpoints`` now takes a ``str`` **or** a list
   * - ``stream.zmq_pub(port)``, ``stream.zmq_pub_etcd(…)``,
       ``stream.zmq_pub_etcd_on(…)``
     - ``zmq_pub(stream, …)`` / ``zmq_pub_etcd(stream, …)`` with an explicit
       ``bind_address`` argument, which subsumes the third form
   * - ``stream.iceoryx2_pub(…)``
     - ``iceoryx2_pub(stream, …)``
   * - ``stream.otlp_push(metric_name, endpoint, service_name)``
     - ``otlp_push(stream, …)`` — and the per-wiring-call memory leak legacy
       took to satisfy a ``&'static str`` bound is gone
   * - ``PrometheusExporter.register(name, stream)``
     - ``PrometheusExporter.gauge(name, stream)`` — the metric type belongs in
       the name, and leaves room for a counter or histogram beside it
   * - ``stream.web_pub(server, topic)``
     - ``server.pub(stream, topic)`` — the handle owns the topic registry, so
       every web entry point lives in one place; ``server.pub_bursts(...)`` is
       new
   * - ``stream.stamp("stage")``
     - ``stamp(stream, "stage")`` — a free function, for uniformity with the
       plugin story (a binding in a third-party crate cannot add a method to the
       ``Stream`` class)

Stream transforms are free functions
------------------------------------

The pattern behind several of the rows above: legacy attached adapter and
tracing transforms as :class:`~wingfoil.Stream` **methods**; wingfoil exposes them as
**module-level functions** taking the stream as the first argument. This is not
cosmetic — a ``#[pyop]`` or ``#[pyadapter]`` in *your* crate cannot add a method
to a ``#[pyclass]`` defined in this one, so making the built-ins free functions
is what makes a third-party binding indistinguishable from a built-in one.

.. code-block:: diff

   - stream.otlp_push("latency", endpoint, "svc")
   + wf.otlp_push(stream, "latency", endpoint, "svc")

Latency tracing
---------------

``latency_report`` now returns a **tuple**:

.. code-block:: diff

   - sink = stream.latency_report(stages)
   + sink, stats = wf.latency_report(stream, stages)
   + print(stats["decode"]["p99_ns"], stats.report())

The engine's ``latency_report`` already hands back a shared ``LatencyStats`` so
a caller can read the numbers after the run; legacy Python could only print
them. The Python surface being the odd one out was the deviation — this removes
it. ``latency_report_if(..., enabled=False)`` likewise returns a never-ticking
sink plus an all-zero stats handle, keeping the return *shape* constant, where
legacy returned the upstream node and changed the type. Recorded as **D13** in
``docs/planning/deviation-register.md``.

Four further latency changes, all of them fixes:

* **Bursts are stamped element-wise** — legacy had no burst shape at all.
* **The stage index is resolved per tick**, not cached off the first value seen.
  Legacy's cached index stamped a differently-staged later value into whatever
  slot happened to sit at that position.
* **``Latency.from_bytes`` validates its stage list** (non-empty, no
  duplicates). Legacy checked only the byte length, so a duplicate name silently
  shadowed a slot that could then never be stamped.
* **A stage-count mismatch at ``latency_report`` is an error.** Legacy
  aggregated over the shorter of the two lists, silently reporting on a prefix.

What you gain
-------------

Not everything is a rename. Wingfoil adds, over the legacy binding:

* **Entry points legacy never bound** — ``kdb_sub`` (the tickerplant tail),
  ``postgres_source``, the ``_with_status`` Aeron pair, ``fix_send`` and
  ``FixConnection.fix_sub``, and ``WebServer.pub_bursts``. Legacy bound all
  fifteen adapters, but not every surface each one has.
* **Full augurs results** — legacy returned only the headline number; the
  prediction intervals, outlier scores and detected periods are now reachable,
  along with the ``model`` selector and the tuning knobs.
* **Back-pressure bounds** (``buffer_size``) on the sinks and on the CSV replay
  look-ahead, so a huge file is not read into memory up front.
* **Web TLS, ``stop()``** and server introspection.
* **The plugin seam** — ``#[pyop]`` / ``#[pygraph]`` / ``#[pyadapter]`` let you
  author components in Rust and compose them from Python, and
  ``compiled_island`` lets a hot sub-graph run as monomorphized straight-line
  code under dynamic Python wiring. Legacy had no equivalent.

Known gaps
----------

* **``dataframe()`` materialises on the run's last cycle.** It assembles the
  frame when :meth:`~wingfoil.Stream.dataframe` is cycled *and* the kernel
  says this is the final cycle — so a stream that has already gone quiet by then
  (a slower ticker, a stream behind ``limit``) never reaches the build step and
  its value stays ``None``. Legacy's ``dataframe()`` re-emitted its rows on every
  tick and had no such edge; wingfoil's equivalent of that shape is
  :meth:`~wingfoil.Stream.collect`, so nothing is lost — reach for
  ``collect()`` when a stream may be silent at the end, including as a column of
  ``build_dataframe``.
* **ZeroMQ cross-language interop** with a *legacy* Rust/Python peer is not
  guaranteed: wingfoil's ``bincode`` envelope is its own. Two wingfoil peers
  interoperate, and so does a wingfoil Python peer with a wingfoil Rust peer publishing
  the same type. Tracked as **C2** in ``docs/planning/deviation-register.md``.

The live register of every deliberate deviation — Python and Rust alike, each
with a class and a ruling — is ``docs/planning/deviation-register.md``. If something
here surprises you, it is worth checking there first.
