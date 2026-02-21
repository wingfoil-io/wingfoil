import pandas as pd
from typing import Any, Dict, List, Union


def to_dataframe(data: Union[List[Any], Dict[str, List[Any]]]) -> pd.DataFrame:
    """
    Converts a list of data (dicts, objects or primitives) or a dict of lists into a DataFrame.
    """
    if not data:
        return pd.DataFrame()
    
    if isinstance(data, dict):
        return pd.DataFrame(data)

    first_item = data[0]
    
    if isinstance(first_item, dict):
        return pd.DataFrame(data)
    
    elif hasattr(first_item, '__dict__'):
        return pd.DataFrame([vars(item) for item in data])
    
    else:
        return pd.DataFrame(data, columns=['value'])

def stream_to_dataframe(stream_or_dict, **run_kwargs):
    """
    Convenience function that runs a stream (or dict of streams) and returns a DataFrame.
    """
    

    if isinstance(stream_or_dict, dict):
        stream = _zip_streams(stream_or_dict)
        stream = stream.collect()
    else:
        stream = stream_or_dict


    stream.run(**run_kwargs)
    
    try:
        data = stream.peek_value()
    except AttributeError:
        print("Warning: Stream was not collected. Attempting to collect and re-run.")
        stream = stream.collect()
        stream.run(**run_kwargs)
        data = stream.peek_value()
        
    return to_dataframe(data)

def _zip_streams(stream_dict: Dict[str, Any]) -> Any:
    """
    Helper: Merges a dict of streams into a single stream of dicts using recursive bimaps.
    """

    from . import _wingfoil 
    bimap = _wingfoil.bimap


    sorted_keys = sorted(stream_dict.keys())
    if not sorted_keys:
        raise ValueError("Cannot convert empty dict of streams")

    first_key = sorted_keys[0]
    merged_stream = stream_dict[first_key].map(lambda x: {first_key: x})

    for key in sorted_keys[1:]:
        next_stream = stream_dict[key]
        merged_stream = bimap(
            merged_stream,
            next_stream,
            lambda acc, val, k=key: {**acc, k: val}
        )
        
    return merged_stream