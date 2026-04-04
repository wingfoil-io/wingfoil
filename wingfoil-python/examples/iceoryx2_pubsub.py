from wingfoil import ticker, iceoryx2_sub, Iceoryx2ServiceVariant, Iceoryx2Mode, Graph
import time

def main():
    service_name = "wingfoil/python/test"
    
    # Create a subscriber stream
    # Note: py_iceoryx2_sub returns a stream of list[bytes]
    sub = iceoryx2_sub(
        service_name, 
        variant=Iceoryx2ServiceVariant.Local,
        mode=Iceoryx2Mode.Spin
    )
    
    # Collect and print received messages
    collected = sub.collapse().inspect(lambda msg: print(f"Python received: {msg}"))
    
    # Create a publisher node
    # The stream should yield bytes or list[bytes]
    pub = ticker(0.1).map(lambda _: b"hello from python").iceoryx2_pub(
        service_name,
        variant=Iceoryx2ServiceVariant.Local
    )
    
    # Run both in a graph
    print("Starting iceoryx2 python pub/sub (Local variant)...")
    graph = Graph([pub, collected])
    graph.run(duration=0.5)
    print("Done.")

if __name__ == "__main__":
    main()
