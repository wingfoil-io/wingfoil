import pytest
import pandas as pd
from wingfoil.pandas_helpers import to_dataframe, build_dataframe
from wingfoil import ticker, Graph

class SimpleObj:
    def __init__(self, a, b):
        self.a = a
        self.b = b

def test_empty_list():
    df = to_dataframe([])
    assert df.empty

def test_to_dataframe_with_time_tuples():
    data = [
        (1.0, {"price": 100, "qty": 10}), 
        (2.0, {"price": 101, "qty": 5})
    ]
    df = to_dataframe(data)
    
    assert len(df) == 2
    assert "time" in df.columns
    assert "price" in df.columns
    assert df.iloc[0]["time"] == 1.0
    assert df.iloc[0]["price"] == 100

def test_to_dataframe_legacy_objects():
    # Backward compatibility check for raw objects
    data = [SimpleObj(1, 2), SimpleObj(3, 4)]
    df = to_dataframe(data)
    assert len(df) == 2
    assert "a" in df.columns
    assert df.iloc[0]["a"] == 1

def test_to_dataframe_legacy_primitives():
    # Backward compatibility check for raw primitives
    data = [10.5, 11.0, 12.5]
    df = to_dataframe(data)
    assert len(df) == 3
    assert df.columns[0] == "value"
    assert df.iloc[0]["value"] == 10.5
    
def test_dict_of_streams():
    source = ticker(0.01).count().limit(3)
    
    stream_a = source.map(lambda i: i - 1).dataframe()
    stream_b = source.map(lambda i: (i - 1) * 2).dataframe()

    data = {
        "col_a": stream_a,
        "col_b": stream_b
    }
    
    Graph([stream_a, stream_b]).run(realtime=False)
  
    df = build_dataframe(data)

    assert len(df) == 3
    assert "time" in df.columns
    assert "col_a" in df.columns
    assert "col_b" in df.columns
    
    assert df.iloc[0]["col_a"] == 0
    assert df.iloc[0]["col_b"] == 0
    assert df.iloc[2]["col_a"] == 2
    assert df.iloc[2]["col_b"] == 4
import numpy as np

def test_async_frequencies():
    """
    Proves that two entirely independent tickers running at different speeds
    can be executed by the Graph and properly outer-joined by Pandas,
    filling missing timestamps with NaNs.
    """
   
    fast_source = ticker(0.01).count().limit(4)
    slow_source = ticker(0.02).count().limit(2)
    
    fast_stream = fast_source.map(lambda x: x * 10).dataframe()
    slow_stream = slow_source.map(lambda x: x * 100).dataframe()
    

    Graph([fast_stream, slow_stream]).run(realtime=False)
    
    df = build_dataframe({"fast": fast_stream, "slow": slow_stream})
  
    assert len(df) == 4
    assert "fast" in df.columns
    assert "slow" in df.columns
    
 
    assert pd.isna(df.iloc[1]["slow"])
    assert pd.isna(df.iloc[3]["slow"])
    # But it should have values where they align
    assert not pd.isna(df.iloc[0]["slow"])

def test_massive_fan_out():
    """
    Proves the Rust engine correctly evaluates a single source splitting
    into multiple branches, and Python correctly aligns them all.
    """
    source = ticker(0.01).count().limit(3)
    
    s_add = source.map(lambda x: x + 5).dataframe()
    s_sub = source.map(lambda x: x - 5).dataframe()
    s_mult = source.map(lambda x: x * 5).dataframe()
    
    Graph([s_add, s_sub, s_mult]).run(realtime=False)
    
    df = build_dataframe({
        "add": s_add, 
        "sub": s_sub, 
        "mult": s_mult
    })
    
    assert len(df) == 3
    assert all(col in df.columns for col in ["time", "add", "sub", "mult"])
    
    assert df.iloc[2]["add"] == 8
    assert df.iloc[2]["sub"] == -2
    assert df.iloc[2]["mult"] == 15


def test_to_dataframe_time_tuple_with_object_values():
    """to_dataframe with (time, obj) tuples where obj has __dict__ (line 19-21)."""
    data = [
        (1.0, SimpleObj(a=10, b=20)),
        (2.0, SimpleObj(a=30, b=40)),
    ]
    df = to_dataframe(data)
    assert len(df) == 2
    assert "time" in df.columns
    assert "a" in df.columns
    assert "b" in df.columns
    assert df.iloc[0]["time"] == 1.0
    assert df.iloc[0]["a"] == 10
    assert df.iloc[1]["b"] == 40


def test_build_dataframe_skips_empty_streams():
    """build_dataframe skips streams whose peek_value() is falsy (lines 48-50)."""
    # A stream that ran 0 cycles has an empty list as peek_value
    empty_stream = ticker(0.01).count().limit(3).dataframe()
    live_stream = ticker(0.01).count().limit(3).dataframe()

    # Only run live_stream — empty_stream stays empty
    live_stream.run(realtime=False)

    df = build_dataframe({"empty": empty_stream, "live": live_stream})
    # Only "live" column should appear (empty_stream's val is falsy)
    assert "live" in df.columns
    assert len(df) == 3


def test_to_dataframe_with_dict_list():
    """to_dataframe with a plain list of dicts (lines 31-32)."""
    data = [{"x": 1, "y": 2}, {"x": 3, "y": 4}]
    df = to_dataframe(data)
    assert len(df) == 2
    assert "x" in df.columns
    assert df.iloc[0]["x"] == 1


def test_to_dataframe_with_object_list():
    """to_dataframe with a plain list of objects with __dict__ (lines 33-34)."""
    data = [SimpleObj(a=5, b=6), SimpleObj(a=7, b=8)]
    df = to_dataframe(data)
    assert len(df) == 2
    assert "a" in df.columns
    assert df.iloc[1]["b"] == 8


def test_to_dataframe_with_raw_dict():
    """to_dataframe with a plain dict (lines 27-28)."""
    data = {"col1": [1, 2, 3], "col2": [4, 5, 6]}
    df = to_dataframe(data)
    assert len(df) == 3
    assert "col1" in df.columns
    assert df.iloc[2]["col2"] == 6
