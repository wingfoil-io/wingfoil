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
