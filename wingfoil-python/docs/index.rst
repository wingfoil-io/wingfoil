Wingfoil
========

Wingfoil is a blazingly fast, highly scalable stream processing framework
designed for latency-critical use cases such as electronic trading and real-time
AI systems. You define a graph of transformations over streams; Wingfoil drives
their execution in a tightly scheduled DAG, either against live data or
replayed history.

The Rust engine does the heavy lifting; the ``wingfoil`` Python package exposes
the same graph model, operators, and production-ready I/O adapters from Python.

.. toctree::
   :maxdepth: 2
   :caption: User Guide

   readme

.. toctree::
   :maxdepth: 2
   :caption: API Reference

   api
