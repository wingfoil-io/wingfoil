# ZMQ Python Examples

```
zmq/
  direct/  — hardcoded address, no discovery infra needed
  etcd/    — etcd-based service discovery (requires etcd)
```

## direct/

Run publisher and subscriber in separate terminals:

```sh
python examples/zmq/direct/zmq_pub.py
python examples/zmq/direct/zmq_sub.py
```

## etcd/

Start etcd first:

```sh
docker run --rm -p 2379:2379 \
  -e ETCD_LISTEN_CLIENT_URLS=http://0.0.0.0:2379 \
  -e ETCD_ADVERTISE_CLIENT_URLS=http://0.0.0.0:2379 \
  gcr.io/etcd-development/etcd:v3.5.0
```

Then run publisher and subscriber in separate terminals:

```sh
python examples/zmq/etcd/zmq_pub.py
python examples/zmq/etcd/zmq_sub.py
```
