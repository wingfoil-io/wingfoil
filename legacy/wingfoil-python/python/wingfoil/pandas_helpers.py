import pandas as pd
from typing import Any, Dict, List, Union

def to_dataframe(data: Union[List[Any], Dict[str, List[Any]]]) -> pd.DataFrame:
    """
    Converts a list of data into a DataFrame.
    """
    if not data:
        return pd.DataFrame()
    
    if isinstance(data, list) and isinstance(data[0], tuple) and len(data[0]) == 2:
        times = [row[0] for row in data]
        values = [row[1] for row in data]
        
        if isinstance(values[0], dict):
            df = pd.DataFrame(values)
            df.insert(0, 'time', times)
            return df
        elif hasattr(values[0], '__dict__'):
            df = pd.DataFrame([vars(item) for item in values])
            df.insert(0, 'time', times)
            return df

        else:
            return pd.DataFrame({'time': times, 'value': values})
    
    if isinstance(data, dict):
        return pd.DataFrame(data)

    first_item = data[0]
    if isinstance(first_item, dict):
        return pd.DataFrame(data)
    elif hasattr(first_item, '__dict__'):
        return pd.DataFrame([vars(item) for item in data])
    else:
        return pd.DataFrame(data, columns=['value'])

def build_dataframe(stream_dict: Dict[str, Any]) -> pd.DataFrame:
    """
    Builds a combined DataFrame from a dict of streams.
    """
    
    dataframes = []
    
    for col_name, stream in stream_dict.items():
        val = stream.peek_value()
        if not val:
            continue
        
        times = [row[0] for row in val]
        values = [row[1] for row in val]
        
        df = pd.DataFrame({'time': times, col_name: values})
        df.set_index('time', inplace=True)
        dataframes.append(df)
    
    if not dataframes:
        return pd.DataFrame()
    
    final_df = pd.concat(dataframes, axis=1, join='outer')
    
    return final_df.reset_index()