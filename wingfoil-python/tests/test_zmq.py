import threading
import time

import pytest
import zmq

import wingfoil as wf

PORT = 5570


def _publish_garbage(port, ready):
    """Publish raw bytes that cannot be deserialized as Message<Vec<u8>>."""
    ctx = zmq.Context()
    sock = ctx.socket(zmq.PUB)
    sock.bind(f"tcp://127.0.0.1:{port}")
    ready.set()
    time.sleep(0.3)  # let subscriber connect (slow-joiner)
    for _ in range(20):
        sock.send(b"not valid bincode")
        time.sleep(0.05)
    sock.close()
    ctx.term()


def test_deserialization_error_propagates():
    ready = threading.Event()
    t = threading.Thread(target=_publish_garbage, args=(PORT, ready), daemon=True)
    t.start()
    ready.wait()

    data, _status = wf.py_zmq_sub(f"tcp://127.0.0.1:{PORT}")
    data_node = data.inspect(lambda _: None)

    with pytest.raises(Exception):
        wf.Graph([data_node]).run(duration=3.0)
