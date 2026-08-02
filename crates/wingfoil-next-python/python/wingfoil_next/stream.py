"""Python-defined graph nodes, in the classic `CustomStream` shape.

The engine already accepts a Python object as a node via
``Graph.custom_node(upstreams, obj)``, whose protocol is ``cycle(values) ->
bool`` plus ``peek()``. That is the composition form. This module adds the
*inheritance* form legacy wingfoil exposes — subclass, implement ``cycle()``,
read upstreams off ``self`` — on top of it:

    class MyStream(CustomStream):
        def cycle(self):
            total = sum(u.peek_value() for u in self.upstreams())
            self.set_value(total)
            return True

    stream = MyStream(graph, [a, b])   # already a Stream — chain off it
    stream.map(lambda x: x * 2).print()

As in legacy, ``__new__`` returns the wired ``Stream`` rather than the subclass
instance, so the constructor call *is* the wiring step and the result chains
like any other stream. The instance stays alive as the node's state.

Two deviations from legacy, both forced by the engine rather than chosen:

* **The graph is explicit.** ``MyStream(graph, upstreams)``, not
  ``MyStream(upstreams)``. Next has no ambient graph — every source is built
  from a ``Graph`` — and a ``Stream`` does not carry a reference back to its
  builder, so there is nothing to infer it from.
* **``upstreams()`` yields value snapshots, not the upstream ``Stream``
  objects.** During a run the engine holds its runner mutably borrowed, so a
  Python ``cycle`` cannot call back into the graph to read a sibling stream;
  the current values are handed in instead and wrapped here in objects
  exposing ``peek_value()``, which is the only stream method legacy's
  ``cycle`` implementations use. A not-yet-ticked upstream reads as ``None``.

A graph containing a Python-defined node is **single-run**: the instance's
state is owned by the caller, and the engine has no hook to reset it between
runs, so a second ``run()`` raises rather than replaying from dirty state.
"""

from __future__ import annotations

from typing import Any, Iterable, List, Sequence

__all__ = ["CustomStream"]


class UpstreamValue:
    """One upstream's value for the current cycle.

    Stands in for the upstream ``Stream`` in :meth:`CustomStream.upstreams`,
    exposing the ``peek_value()`` legacy ``cycle`` bodies call. It is a snapshot
    — it does not track the stream after the cycle that produced it.
    """

    __slots__ = ("_value",)

    def __init__(self, value: Any) -> None:
        self._value = value

    def peek_value(self) -> Any:
        """This upstream's current value (``None`` if it has not ticked)."""
        return self._value

    def __repr__(self) -> str:
        return f"UpstreamValue({self._value!r})"


class _NodeAdapter:
    """Bridges the engine's ``cycle(values)`` protocol to a no-arg ``cycle()``.

    The engine hands the upstream values to ``cycle``; legacy's signature takes
    none and reads them off ``self``. Passing this adapter to ``custom_node``
    rather than the node itself keeps the subclass's ``cycle(self)`` signature
    exactly as legacy wrote it — the two never collide.
    """

    __slots__ = ("_node",)

    def __init__(self, node: "CustomStream") -> None:
        self._node = node

    def cycle(self, values: Sequence[Any]) -> bool:
        self._node._values = list(values)
        return bool(self._node.cycle())

    def peek(self) -> Any:
        return self._node.peek()


class CustomStream:
    """Base class for a Python-defined graph node.

    Subclass it, implement :meth:`cycle`, and construct it with the graph and
    the upstream streams; the constructor returns the wired ``Stream``.
    """

    def __new__(
        cls,
        graph: Any,
        upstreams: Iterable[Any],
        *args: Any,
        **kwargs: Any,
    ) -> Any:
        node = super().__new__(cls)
        # Seeded before the subclass's __init__ so it may already call
        # set_value() (legacy nodes sometimes seed an initial value there).
        node._values = []
        node._value = None
        # Python only calls __init__ automatically when __new__ returns an
        # instance of cls; we return a Stream, so invoke it ourselves — the
        # graph/upstreams are wiring, so only the remaining args are passed on.
        node.__init__(*args, **kwargs)
        return graph.custom_node(list(upstreams), _NodeAdapter(node))

    def upstreams(self) -> List[UpstreamValue]:
        """The upstreams' current values, in the order they were wired."""
        return [UpstreamValue(value) for value in self._values]

    def cycle(self) -> bool:
        """Advance the node; return whether it ticked this cycle.

        Implemented by the subclass. Call :meth:`set_value` with the new value
        before returning ``True``; return ``False`` to stay quiet, leaving the
        previous value in place and not ticking downstream.
        """
        raise NotImplementedError(
            f"{type(self).__name__} must implement cycle(self) -> bool"
        )

    def peek(self) -> Any:
        """The node's current value — read by the engine when `cycle` ticked."""
        return self._value

    def set_value(self, value: Any) -> None:
        """Set the value this node emits for the current cycle."""
        self._value = value
