# etcd Adapter Example

Demonstrates a full round-trip with the etcd adapter:

1. **Seed** — writes two keys to `/example/source/` via `etcd_pub`
2. **Round-trip** — watches `/example/source/` with `etcd_sub`, uppercases each value, writes to `/example/dest/` via `etcd_pub`

## Setup

### Local (Docker)

```sh
docker run --rm -p 2379:2379 \
  -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
  -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
  gcr.io/etcd-development/etcd:v3.5.0
```

### Kubernetes

Deploy a single-node etcd pod and expose it as a `ClusterIP` service:

```yaml
apiVersion: apps/v1
kind: StatefulSet
metadata:
  name: etcd
spec:
  selector:
    matchLabels:
      app: etcd
  serviceName: etcd
  replicas: 1
  template:
    metadata:
      labels:
        app: etcd
    spec:
      containers:
        - name: etcd
          image: gcr.io/etcd-development/etcd:v3.5.0
          ports:
            - containerPort: 2379
          env:
            - name: ETCD_LISTEN_CLIENT_URLS
              value: http://0.0.0.0:2379
            - name: ETCD_ADVERTISE_CLIENT_URLS
              value: http://etcd:2379
---
apiVersion: v1
kind: Service
metadata:
  name: etcd
spec:
  selector:
    app: etcd
  ports:
    - port: 2379
      targetPort: 2379
```

Connect from within the cluster using `http://etcd:2379`:

```rust
let conn = EtcdConnection::new("http://etcd:2379");
```

For a production cluster with multiple replicas, pass all endpoints:

```rust
let conn = EtcdConnection::with_endpoints([
    "http://etcd-0.etcd:2379",
    "http://etcd-1.etcd:2379",
    "http://etcd-2.etcd:2379",
]);
```

## Run

```sh
cargo run --example etcd --features etcd
```

## Code

```rust
use wingfoil::adapters::etcd::*;
use wingfoil::*;

const ENDPOINT: &str = "http://localhost:2379";
const SOURCE_PREFIX: &str = "/example/source/";
const DEST_PREFIX: &str = "/example/dest/";

fn main() -> anyhow::Result<()> {
    let conn = EtcdConnection::new(ENDPOINT);

    // Write two keys to the source prefix once.
    let seed = constant(burst![
        EtcdEntry { key: format!("{SOURCE_PREFIX}greeting"), value: b"hello".to_vec() },
        EtcdEntry { key: format!("{SOURCE_PREFIX}subject"),  value: b"world".to_vec() },
    ])
    .etcd_pub(conn.clone(), None, true);

    // Watch the source prefix, uppercase each value, write to the dest prefix.
    let round_trip = etcd_sub(conn.clone(), SOURCE_PREFIX)
        .map(|burst| {
            burst
                .into_iter()
                .map(|event| {
                    let dest_key = event.entry.key.replacen(SOURCE_PREFIX, DEST_PREFIX, 1);
                    let upper = event.entry.value_str().unwrap_or("").to_uppercase().into_bytes();
                    println!("  {} → {}", event.entry.key, String::from_utf8_lossy(&upper));
                    EtcdEntry { key: dest_key, value: upper }
                })
                .collect::<Burst<EtcdEntry>>()
        })
        .etcd_pub(conn, None, true);

    Graph::new(vec![seed, round_trip], RunMode::RealTime, RunFor::Cycles(3)).run()?;
    Ok(())
}
```

## Output

```
  /example/source/greeting → HELLO
  /example/source/subject → WORLD
```
