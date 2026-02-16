import pytest
import pandas as pd
from wingfoil.pandas_helpers import to_dataframe

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
    