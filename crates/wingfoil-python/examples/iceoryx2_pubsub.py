#!/usr/bin/env python3
"""iceoryx2 pub/sub in one process, over shared memory.

Publisher and subscriber are wired into the same graph on the `local` service
variant (in-process), so this runs without a separate peer. Switch `variant` to
`"ipc"` on both to talk to another process — or to the Rust
`iceoryx2` example, which speaks the same byte-slice service.

`mode="signaled"` blocks the subscriber's thread until the publisher signals,
rather than spinning: the lowest-CPU of the three modes.

    cd crates/wingfoil-python && maturin develop --features iceoryx2
    python examples/iceoryx2_pubsub.py

Ported from legacy wingfoil's `examples/iceoryx2_pubsub.py`; the variant and
mode selectors are plain strings here rather than enum classes.
"""

import wingfoil as wf

SERVICE = "wingfoil/python/example/counter"
VARIANT = "local"

g = wf.Graph()

received = (
    wf.iceoryx2_sub(g, SERVICE, variant=VARIANT, mode="signaled")
    .inspect(lambda msgs: print(f"received: {msgs}"))
    .collect()
)

wf.iceoryx2_pub(
    g.counter(period_nanos=100_000_000).map(lambda n: f"hello {n}".encode()),
    SERVICE,
    variant=VARIANT,
)

print("Publishing and subscribing for half a second...")
g.run(realtime=True, duration_nanos=500_000_000)
print("Done.")
