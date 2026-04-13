from wingfoil import ticker, iceoryx2_sub, Iceoryx2ServiceVariant, Iceoryx2Mode, Graph
import time

def main():
    service_name = "wingfoil/python/test/v2"
    
    # Create a subscriber stream using Signaled mode (true blocking)
    sub = iceoryx2_sub(
        service_name, 
        variant=Iceoryx2ServiceVariant.Local,
        mode=Iceoryx2Mode.Signaled
    )
    
    # Inspect and collect received bursts. Each tick yields a list of `bytes`.
    collected = sub.inspect(lambda msgs: print(f"Python received: {msgs}")).collect()
    
    # Create a publisher node (Signaled mode is triggered automatically by publisher)
    pub = ticker(0.1).count().map(lambda _: b"hello from python slice").iceoryx2_pub(
        service_name,
        variant=Iceoryx2ServiceVariant.Local
    )
    
    # Run both in a graph
    print("Starting iceoryx2 python signaled slice pub/sub (Local variant)...")
    graph = Graph([pub, collected])
    graph.run(realtime=True, duration=0.5)
    print("Done.")

if __name__ == "__main__":
    main()
