"""Smoke-test the runnable examples: each must execute without raising.

Keeps `examples/*.py` working as the binding evolves. The examples print to
stdout (that's their point); we only assert they run clean.
"""

import pathlib
import runpy

import pytest

EXAMPLES = pathlib.Path(__file__).resolve().parent.parent / "examples"


@pytest.mark.parametrize(
    "name", ["quick_start", "custom_stream", "custom_stream_subclass", "plugin_sdk"]
)
def test_example_runs(name):
    runpy.run_path(str(EXAMPLES / f"{name}.py"), run_name="__main__")


def test_dataframe_example_runs():
    pytest.importorskip("pandas")
    runpy.run_path(str(EXAMPLES / "dataframe.py"), run_name="__main__")
