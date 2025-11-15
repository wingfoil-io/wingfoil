

from ._wingfoil import PyStream


class Stream():

    def __new__(cls, *args, **kwargs):
        """Overriden constructor to wrap this instance 
        in proxy Stream - this is where the magic happens"""
        obj = super().__new__(cls)
        obj.__init__(*args, **kwargs)
        proxy = PyStream(obj)
        print("proxy %s" % proxy)
        return proxy

    def __init__(self, upstreams = None):
        self._value = None
        self._upstreams = upstreams or []

    def upstreams(self):
        return self._upstreams

    def cycle(self):
        raise Exception("cycle must be implemented in derived class")

    def peek(self):
        return self._value

    def set_value(self, value):
        self._value = value