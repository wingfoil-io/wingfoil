import pandas as pd

def to_dataframe(data):
    """
    Converts a list of data (dicts, objects or primitives) into a Pandas DataFrame.
    """
    
    if not data:
        return pd.DataFrame()
    
    first_item = data[0]
    
    if isinstance(first_item, dict):
        return pd.DataFrame(data)
    
    elif hasattr(first_item, '__dict__'):
        return pd.DataFrame([vars(item) for item in data])
    
    else:
        return pd.DataFrame(data, columns=['value'])
    

def stream_to_dataframe(stream, **run_kwargs):
    """
    Convenience function that runs a stream that returns the result as a DataFrame.
    
    Args:
        stream: The wingfoil stream object
        **run_kwargs: Arguments passed to stream.run() (e.g., cycles=10, realtime=False)
    """
    
    stream.run(**run_kwargs)
    data = stream.peek_value()
    
    return to_dataframe(data)
    
        