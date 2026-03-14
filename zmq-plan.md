# ZMQ Adapter — Implementation Plan

## Overview

Two high-priority issues to fix in `wingfoil/src/adapters/zmq.rs`:

1. **First message dropped** — ZMQ slow-joiner bug: publisher sends before subscriber's subscription is registered ✅
2. **Status reporting** — subscriber emits no connect/disconnect events on-graph ✅

### Design

- Add `ZmqEvent<T>` enum (Data or Status) sent through the in-process channel
- `zmq_sub` internally splits the event stream using `MapFilterStream` and returns a plain tuple `(data_stream, status_stream)`
- No custom node types, no wrapper struct — `MapFilterStream` handles the filtering
- Wire protocol is **unchanged** — `Message<T>` still goes over ZMQ; `ZmqEvent` wrapping is purely in-process

---

## Remaining Tasks

### Task 6 — Add zmq_reports_connected_status test

Port 5561. Only asserts `Connected` — `Disconnected` is sent during shutdown after graph cycles end, so not reliably observable in a same-graph test.

```rust
#[test]
fn zmq_reports_connected_status() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(50);
    let port = 5561;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 10);
    let (data, status) = zmq_sub::<u64>(&address);
    let data_node = data.collect().finally(|res, _| {
        let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
        assert!(!values.is_empty(), "no data received");
        Ok(())
    });
    let status_node = status.collect().finally(|statuses, _| {
        let vs: Vec<ZmqStatus> = statuses.into_iter().map(|item| item.value).collect();
        assert!(vs.contains(&ZmqStatus::Connected), "expected Connected, got: {vs:?}");
        Ok(())
    });
    Graph::new(vec![sender(period, port), data_node, status_node], RunMode::RealTime, run_for)
        .run()
        .unwrap();
}
```

---

## Notes

- Slow-joiner fix uses `.delay(Duration::from_millis(200))` in the sender (not a sleep in `start()`)
- `ZmqEvent::Disconnected` is observable mid-run (e.g. publisher crashes) but not in same-graph shutdown tests
- All existing test ports (5556–5560) are in use; Task 6 uses 5561
- `ZeroMqPub` trait and `ZeroMqSenderNode` are unchanged
