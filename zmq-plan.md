# ZMQ Adapter — Implementation Plan

## Overview

Two high-priority issues to fix in `wingfoil/src/adapters/zmq.rs`:

1. **First message dropped** — ZMQ slow-joiner bug: publisher sends before subscriber's subscription is registered
2. **Status reporting** — subscriber emits no connect/disconnect events on-graph

### Design

- Add `ZmqEvent<T>` enum (Data or Status) sent through the in-process channel
- `zmq_sub` returns `Rc<dyn Stream<Burst<ZmqEvent<T>>>>`
- `unpack()` splits this into two streams: `.data` and `.status`
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

### Task 3 — Update zmq_sub return type

```rust
pub fn zmq_sub<T: Element + Send + DeserializeOwned>(
    address: &str,
) -> Rc<dyn Stream<Burst<ZmqEvent<T>>>> {
    let subscriber = ZeroMqSubscriber::new(address.to_string());
    let f = move |channel_sender| subscriber.run(channel_sender);
    ReceiverStream::new(f, true).into_stream()
}
```

`ReceiverStream<ZmqEvent<T>>` satisfies `StreamPeekRef<Burst<ZmqEvent<T>>>`, so `into_stream()` returns `Rc<dyn Stream<Burst<ZmqEvent<T>>>>`.

---

### Task 4 — Add ZmqDataNode and ZmqStatusNode

Two internal nodes that split the event stream:

**ZmqDataNode\<T\>**
- `src: Rc<dyn Stream<Burst<ZmqEvent<T>>>>`
- `value: Burst<T>` (default)
- `cycle()`: filter_map Data variants; tick if non-empty
- `StreamPeekRef<Burst<T>>`

**ZmqStatusNode\<T\>**
- `src: Rc<dyn Stream<Burst<ZmqEvent<T>>>>`
- `value: ZmqStatus` (default = Disconnected)
- `cycle()`: find_map Status variants; tick if found
- `StreamPeekRef<ZmqStatus>`

Both use `#[derive(new)]` with `#[new(default)]` on value fields. `into_stream()` blanket impls in `types.rs` handle the rest.

---

### Task 5 — Add ZmqStreams struct and ZmqUnpack trait

```rust
pub struct ZmqStreams<T: Element + Send> {
    pub data: Rc<dyn Stream<Burst<T>>>,
    pub status: Rc<dyn Stream<ZmqStatus>>,
}

pub trait ZmqUnpack<T: Element + Send> {
    fn unpack(self) -> ZmqStreams<T>;
}

impl<T: Element + Send> ZmqUnpack<T> for Rc<dyn Stream<Burst<ZmqEvent<T>>>> {
    fn unpack(self) -> ZmqStreams<T> {
        let data = ZmqDataNode::new(self.clone()).into_stream();
        let status = ZmqStatusNode::new(self).into_stream();
        ZmqStreams { data, status }
    }
}
```

Both nodes hold `Rc` clones of the same source. The graph deduplicates by pointer identity.

---

### Task 6 — Fix ZMQ slow-joiner (first message dropped)

In `ZeroMqSenderNode::start()`, sleep 100ms after `bind()`:

```rust
socket.bind(&address)?;
std::thread::sleep(std::time::Duration::from_millis(100));
self.socket = Some(socket);
```

The subscriber thread is spawned in `ReceiverStream::setup()` (before `start()`), so 100ms is enough for `connect()` + `set_subscribe()` + ZMQ subscription handshake to complete before the first publish.

---

### Task 7 — Add zmq_first_message_not_dropped test

Fails before task 6, passes after. Port 5560.

```rust
#[test]
fn zmq_first_message_not_dropped() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(50);
    let port = 5560;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 15);
    let recv_node = zmq_sub::<u64>(&address)
        .unpack()
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

### Task 8 — Add zmq_reports_connected_status test

Port 5561. Only asserts `Connected` — `Disconnected` is sent during shutdown after graph cycles end, so not reliably observable in a same-graph test.

```rust
#[test]
fn zmq_reports_connected_status() {
    _ = env_logger::try_init();
    let period = Duration::from_millis(50);
    let port = 5561;
    let address = format!("tcp://127.0.0.1:{port}");
    let run_for = RunFor::Duration(period * 10);
    let ZmqStreams { data, status } = zmq_sub::<u64>(&address).unpack();
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

### Task 9 — Update existing tests

1. Update `receiver()` helper to call `.unpack().data` before `.logged()`:
   ```rust
   fn receiver(address: &str) -> Rc<dyn Node> {
       zmq_sub::<u64>(address)
           .unpack()
           .data
           .logged("sub", Info)
           .collect()
           .finally(|res, _| { /* unchanged */ })
   }
   ```
2. `zmq_pub_historical_mode_fails` — no changes needed (uses `sender()` / `ZeroMqPub`)
3. `zmq_sub_historical_mode_fails` — no changes needed (`as_node()` works on new return type)
4. Remove stale commented-out code blocks (original lines 34–73 and 135–184)

---

### Task 10 — cargo fmt + clippy

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
