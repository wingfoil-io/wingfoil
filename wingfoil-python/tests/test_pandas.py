import pytest
import pandas as pd
from wingfoil.pandas_helpers import to_dataframe. stream_to_dataframe
from wingfoil import ticker

class SimpleObj:
    def __init__(self, a, b):
        self.a = a
        self.b = b

def test_empty_list():
    df = to_dataframe([])
    assert df.empty

def test_list_of_dicts():
    data = [{"price": 100, "qty": 10}, {"price": 101, "qty": 5}]
    df = to_dataframe(data)
    assert len(df) == 2
    assert "price" in df.columns
    assert df.iloc[0]["price"] == 100

def test_list_of_objects():
    data = [SimpleObj(1, 2), SimpleObj(3, 4)]
    df = to_dataframe(data)
    assert len(df) == 2
    assert "a" in df.columns
    assert df.iloc[0]["a"] == 1

def test_list_of_primitives():
    data = [10.5, 11.0, 12.5]
    df = to_dataframe(data)
    assert len(df) == 3
    assert df.columns[0] == "value"
    assert df.iloc[0]["value"] == 10.5
    
def test_dict_of_streams():
    source = ticker(0.01).count().limit(3)

    data = {
        "col_a": source.map(lambda i: i),
        "col_b": source.map(lambda i: i * 2)
    }
    df = stream_to_dataframe(data, realtime=True)

    assert len(df) == 3
    assert "col_a" in df.columns
    assert "col_b" in df.columns
    assert df.iloc[0]["col_a"] == 0
    assert df.iloc[0]["col_b"] == 0
    assert df.iloc[2]["col_a"] == 2
    assert df.iloc[2]["col_b"] == 4
    