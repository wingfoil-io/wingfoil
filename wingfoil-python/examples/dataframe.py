from wingfoil import ticker, stream_to_dataframe, to_dataframe

def run_example():

    print("~~~ Primitives to DataFrame ~~~")
    stream = ticker(0.01).count().limit(5).collect()
    
    df_prim = stream_to_dataframe(stream, realtime=True)
    print(df_prim)

    
    print("\n~~~ Dicts to DataFrame (Explicit) ~~~")
    stream = ticker(0.01).count().map(
        lambda i: {"price": 100 + i, "qty": 10}
    ).limit(5).collect()
    
    stream.run(realtime=True)
    df_dict = to_dataframe(stream.peek_value())
    print(df_dict)
    

    print("\n~~~ Dicts to DataFrame (Convenience) ~~~")
    df_auto = stream_to_dataframe(
        ticker(0.01).count().map(lambda i: {"id": i, "val": i*2}).limit(5).collect(),
        realtime=True
    )
    print(df_auto)

if __name__ == "__main__":
    run_example()