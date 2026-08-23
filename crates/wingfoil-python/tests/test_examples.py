"""Smoke-test the runnable examples: each must execute without raising.

Keeps `examples/*.py` working as the binding evolves. The examples print to
stdout (that's their point); we only assert they run clean.

Covers the core examples — every one that runs on a default `maturin develop`
with nothing else present. The adapter examples are excluded by that rule, not
by oversight: each needs a cargo feature this build may not carry, and all but
`iceoryx2_pubsub.py` (self-contained, publisher and subscriber in one graph)
also need a peer or a server. Their coverage is the adapters' own
service-backed test legs.
"""

import pathlib
import runpy

import pytest

EXAMPLES = pathlib.Path(__file__).resolve().parent.parent / "examples"


@pytest.mark.parametrize(
    "name",
    [
        "quick_start",
        "custom_stream",
        "custom_stream_subclass",
        "plugin_sdk",
        "combine",
        "deduplicate",
        "delay_line",
        "latency",
    ],
)
def test_example_runs(name):
    runpy.run_path(str(EXAMPLES / f"{name}.py"), run_name="__main__")


def test_dataframe_example_runs():
    pytest.importorskip("pandas")
    runpy.run_path(str(EXAMPLES / "dataframe.py"), run_name="__main__")
