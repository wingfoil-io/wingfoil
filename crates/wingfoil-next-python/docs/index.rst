Wingfoil Next — Python
======================

Wingfoil is a blazingly fast, highly scalable stream processing framework
designed for latency-critical use cases such as electronic trading and
real-time AI systems. You define a graph of transformations over streams;
Wingfoil drives their execution in a tightly scheduled DAG, either against live
data or replayed history.

**Wingfoil Next** is the Op-pattern engine that replaces the original wingfoil
engine, and ``wingfoil_next`` is its Python binding. The Rust engine does the
heavy lifting; the Python package exposes the same graph model, the combinator
surface, the production I/O adapters and the latency-tracing surface — plus a
plugin seam that lets you author ops, sub-graphs and adapters *in Rust* and
compose them from Python.

.. code-block:: python

   import wingfoil_next as wf

   g = wf.Graph()
   out = g.counter(period_nanos=100).map(lambda n: n * 2)
   g.run(cycles=3)          # deterministic historical replay from t=0
   assert out.value() == 6  # 3rd tick -> 3 * 2

Coming from the original ``wingfoil`` package?
----------------------------------------------

``wingfoil_next`` **supersedes** the legacy ``wingfoil`` Python package — it is
a replacement, not a compatibility facade, and the import name changes. Start
with the :doc:`migration` page: it lists every renamed entry point and every
place where next deliberately behaves differently.

.. toctree::
   :maxdepth: 2
   :caption: User Guide

   readme
   migration

.. toctree::
   :maxdepth: 2
   :caption: API Reference

   api
