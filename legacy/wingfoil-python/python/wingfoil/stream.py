from __future__ import annotations

from ._wingfoil import Stream

from typing import Any, Generic, Iterable, List, Optional, TypeVar

T = TypeVar("T")   # The value type carried by the Stream


class CustomStream(Generic[T]):
    def __new__(cls, *args: Any, **kwargs: Any) -> "Stream[T]": 
        """Override constructor to wrap the instance in a PyStream proxy."""
        obj = super().__new__(cls)
        obj.__init__(*args, **kwargs)
        proxy: Stream[T] = Stream(obj)
        print(f"proxy {proxy}")
        return proxy

    def __init__(self, upstreams: Optional[Iterable["Stream[Any]"]] = None) -> None:
        self._value: Optional[T] = None
        self._upstreams: List[Stream[Any]] = list(upstreams) if upstreams else []

    def upstreams(self) -> List["Stream[Any]"]:
        return self._upstreams

    def cycle(self) -> bool:
        """Must be implemented in subclasses."""
        raise Exception("cycle must be implemented in derived class")

    def peek(self) -> Optional[T]:
        return self._value

    def set_value(self, value: T) -> None:
        self._value = value

