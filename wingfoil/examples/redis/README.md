# Redis Adapter Example

Demonstrates a full Pub/Sub round-trip with the Redis adapter. Because Redis
Pub/Sub is fire-and-forget (only live subscribers receive a message), the three
roles run as separate graphs and the subscribers start before anything is published:

1. **processor** — subscribes to `example-source` with `redis_sub`, uppercases each
   payload, and republishes to `example-dest` with `redis_pub`
2. **verifier** — subscribes to `example-dest` and prints what it receives
3. **publisher** — publishes `hello` and `world` to `example-source`

## Setup

```sh
docker run --rm -p 6379:6379 redis:7-alpine
```

## Run

```sh
cargo run --example redis --features redis
```

## Code

See `main.rs` for the full source.

## Output

```
  example-dest -> HELLO
  example-dest -> WORLD
```
