from wingfoil import ticker, Graph, build_dataframe

def run_example():
    print("~~~ Single Stream (Primitives) ~~~")
   
    data_a = ticker(0.01).count().limit(5).dataframe()
    data_a.run(realtime=False)
    
    df_prim = data_a.peek_value()
    print(df_prim)
    
    print("\n~~~ Single Stream (Dictionaries) ~~~")
    data_b = (
        ticker(0.01)
        .count()
        .limit(5)
        .map(lambda i: {"price": 100 + i, "qty": 10})
        .dataframe()
    )
    
    data_b.run(realtime=False)
    df_dict = data_b.peek_value()
    print(df_dict)
    
    print("\n~~~ Multiple Streams (Graph + build_dataframe) ~~~")
    
    source = ticker(0.01).count().limit(5)
    
  
    stream_price = source.map(lambda i: 100 + i).dataframe()
    stream_qty = source.map(lambda _: 10).dataframe()
    

    print("Executing Rust Graph engine...")
    Graph([stream_price, stream_qty]).run(realtime=False)
    
    df_zipped = build_dataframe({
        "price": stream_price,
        "qty": stream_qty,
    })
    
    print("Final Aligned DataFrame:")
    print(df_zipped)

if __name__ == "__main__":
    run_example()