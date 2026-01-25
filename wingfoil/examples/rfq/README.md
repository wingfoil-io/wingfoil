

## RFQ Example

In this example, we sketch out a Request For Quote (RFQ) responder, receiving messages from [paradigm](https://www.paradigm.co/)
over an async websocket and using the demux pattern to route RFQs to pool of statically wired sub-graphs, where business 
logic can be implemented to respond to them.

This example can be run with:

```bash
RUST_LOG=INFO cargo run --example rfq
```