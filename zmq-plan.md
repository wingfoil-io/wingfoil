# ZMQ Adapter — Implementation Plan

## Overview

Two high-priority issues to fix in `wingfoil/src/adapters/zmq.rs`:

1. **First message dropped** — ZMQ slow-joiner bug: publisher sends before subscriber's subscription is registered
2. **Status reporting** — subscriber emits no connect/disconnect events on-graph

### Design

- Add `ZmqEvent<T>` enum (Data or Status) sent through the in-process channel
- `zmq_sub` internally splits the event stream using `MapFilterStream` and returns `ZmqStreams<T>` directly
- No custom node types needed — `MapFilterStream` handles the filtering
- Wire protocol is **unchanged** — `Message<T>` still goes over ZMQ; `ZmqEvent` wrapping is purely in-process

---

## Tasks

### Task 1 — Add ZmqStatus and ZmqEvent types

Add two new public types to `zmq.rs`:

```rust
#[derive(Debug, Clone, PartialEq, Default)]
pub enum ZmqStatus {
    Connected,
    #[default]
    Disconnected,
}

#[derive(Debug, Clone)]
pub enum ZmqEvent<T> {
    Data(T),
    Status(ZmqStatus),
}

impl<T: Default> Default for ZmqEvent<T> {
    fn default() -> Self { ZmqEvent::Data(T::default()) }
}
```

`ZmqEvent<T>: Element + Send` when `T: Element + Send` — all bounds satisfied by derives + manual Default.

No `Serialize`/`Deserialize` needed — `ZmqEvent` only travels in-process via kanal channels, not over the ZMQ wire.

---

### Task 2 — Update ZeroMqSubscriber to emit status events

Change `ZeroMqSubscriber::run()` from `ChannelSender<T>` to `ChannelSender<ZmqEvent<T>>`.

New logic:
1. Call `connect()` — on success, send `Message::RealtimeValue(ZmqEvent::Status(ZmqStatus::Connected))`
2. Loop calling `recv()` which still returns `Message<T>` (wire format unchanged)
3. Map wire messages:
   - `RealtimeValue(v)` → `RealtimeValue(ZmqEvent::Data(v))`
   - `EndOfStream` → send `RealtimeValue(ZmqEvent::Status(ZmqStatus::Disconnected))`, then `EndOfStream`, then break
   - All other variants → ignore
4. On connect error → propagate

---

### Task 3 — Update zmq_sub to return ZmqStreams<T> directly using MapFilterStream

No custom node types. Use `MapFilterStream` (from `nodes/map_filter.rs`) which takes `Box<dyn Fn(IN) -> (OUT, bool)>` — the bool controls whether the node ticks.

```rust
pub struct ZmqStreams<T: Element + Send> {
    pub data: Rc<dyn Stream<Burst<T>>>,
    pub status: Rc<dyn Stream<ZmqStatus>>,
}

pub fn zmq_sub<T: Element + Send + DeserializeOwned>(address: &str) -> ZmqStreams<T> {
    let events: Rc<dyn Stream<Burst<ZmqEvent<T>>>> = {
        let subscriber = ZeroMqSubscriber::new(address.to_string());
        ReceiverStream::new(move |s| subscriber.run(s), true).into_stream()
    };
    let data = MapFilterStream::new(events.clone(), Box::new(|burst: Burst<ZmqEvent<T>>| {
        let data: Burst<T> = burst.into_iter()
            .filter_map(|e| if let ZmqEvent::Data(v) = e { Some(v) } else { None })
            .collect();
        let ticked = !data.is_empty();
        (data, ticked)
    })).into_stream();
    let status = MapFilterStream::new(events, Box::new(|burst: Burst<ZmqEvent<T>>| {
        match burst.into_iter()
            .find_map(|e| if let ZmqEvent::Status(s) = e { Some(s) } else { None })
        {
            Some(s) => (s, true),
            None => (ZmqStatus::default(), false),
        }
    })).into_stream();
    ZmqStreams { data, status }
}
```

Call sites destructure with field names:
```rust
let ZmqStreams { data, status } = zmq_sub::<u64>(address);
```

`ZmqUnpack` trait is **not needed** — `zmq_sub` returns split streams directly.

---

### Task 4 — Fix ZMQ slow-joiner (first message dropped)

In `ZeroMqSenderNode::start()`, sleep 100ms after `bind()`:

```rust
socket.bind(&address)?;
std::thread::sleep(std::time::Duration::from_millis(100));
self.socket = Some(socket);
```

The subscriber thread is spawned in `ReceiverStream::setup()` (before `start()`), so 100ms is enough for `connect()` + `set_subscribe()` + ZMQ subscription handshake to complete before the first publish.

---

### Task 5 — Add zmq_first_message_not_dropped test

Fails before task 4, passes after. Port 5560.

```rust
#[test]
fn zmq_first_message_not_dropped() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(50);
    let port = 5560;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 15);
    let recv_node = zmq_sub::<u64>(&address)
        .data
        .collect()
        .finally(|res, _| {
            let values: Vec<u64> = res.into_iter().flat_map(|item| item.value).collect();
            assert!(!values.is_empty(), "no values received");
            assert_eq!(values[0], 1, "first message dropped: got {}", values[0]);
            Ok(())
        });
    Graph::new(vec![sender(period, port), recv_node], RunMode::RealTime, run_for)
        .run()
        .unwrap();
}
```

---

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
    let ZmqStreams { data, status } = zmq_sub::<u64>(&address);
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

### Task 7 — Update existing tests

1. Update `receiver()` helper — `zmq_sub` now returns `ZmqStreams`, access `.data`:
   ```rust
   fn receiver(address: &str) -> Rc<dyn Node> {
       zmq_sub::<u64>(address)
           .data
           .logged("sub", Info)
           .collect()
           .finally(|res, _| { /* unchanged */ })
   }
   ```
2. `zmq_pub_historical_mode_fails` — no changes (uses `sender()` / `ZeroMqPub`)
3. `zmq_sub_historical_mode_fails` — needs update: `zmq_sub` no longer returns `Rc<dyn Stream<...>>` directly, so `.as_node()` won't work. Use `.data.as_node()` or `.status.as_node()` instead.
4. Remove stale commented-out code blocks (original lines 34–73 and 135–184)

---

### Task 8 — cargo fmt + clippy

```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features
```

Watch for: unused imports, dead code on `ZmqEvent` variants, missing derive bounds.

---

## Notes

- `ZmqEvent::Disconnected` is observable mid-run (e.g. publisher crashes) but not in same-graph shutdown tests
- All existing test ports (5556–5559) are unchanged; new tests use 5560–5561
- `ZeroMqPub` trait and `ZeroMqSenderNode` are unchanged
- `ZmqUnpack` trait eliminated — not needed since `zmq_sub` returns `ZmqStreams` directly
- `ZmqDataNode` and `ZmqStatusNode` eliminated — `MapFilterStream` covers both cases
