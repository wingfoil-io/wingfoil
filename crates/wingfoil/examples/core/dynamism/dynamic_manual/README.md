## Splicing the graph by hand — `add_upstream` / `remove`

The low-level twin of the [`dynamic_group`](../dynamic_group/) example, and the
wingfoil counterpart to legacy wingfoil's `dynamic-manual` (a hand-rolled
`MutableNode` calling `state.add_upstream()` / `state.remove_node()`).

It builds the identical price book over the identical scenario, but instead of
handing the lifecycle to `Builder::dynamic_group` it drives
`Extension::add_upstream` and `Extension::remove` itself, from the `run_dynamic`
hook, and keeps its own registry as a plain `BTreeMap`.

Three things make this work, and each is a rule worth knowing:

1. **Nodes cannot be built from inside a `cycle` closure.** The lifecycle
   streams deposit their values via a `for_each` tap; the hook — which runs
   *between* cycles, where an `Extension` is available — drains them and splices.
2. **`recycle = true`** schedules the freshly appended region to fire at
   `time + 1`, so it observes the feed's *current* value rather than a
   `Default` — which is why the very first book state already carries a price.
3. **Deletion must take effect within the cycle it is seen.** The tap drops the
   book entry immediately and leaves only the unwiring to the boundary; the
   aggregator declares that tap as a **passive** upstream so the scheduler — not
   luck — orders the removal before the publish. Do it at the boundary instead
   and an instrument whose sibling ticks on the same cycle is republished one
   last time.

```rust,ignore
// Between cycles: splice in the adds, then tear down the deletes.
let filtered = ext.filter_value(self.feed, move |(i, _)| *i == key);
let priced = ext.map(filtered, move |(_, px)| { /* … record into the book */ });
ext.add_upstream(self.aggregator, priced, true, true);
…
ext.remove(priced)?;
ext.remove(filtered)?;
```

```text
price book (dynamic_manual): {inst1=101}
price book (dynamic_manual): {inst1=101, inst2=202}
price book (dynamic_manual): {inst2=204}
price book (dynamic_manual): {inst2=204, inst3=305}
price book (dynamic_manual): {inst3=305, inst4=406}
price book (dynamic_manual): {inst3=307, inst4=406}
price book (dynamic_manual): {inst3=307, inst4=408}
price book (dynamic_manual): {inst4=408, inst5=509}
price book (dynamic_manual): {inst4=410, inst5=509}
price book (dynamic_manual): {inst4=410, inst5=511}
price book (dynamic_manual): {inst5=511, inst6=612}
price book (dynamic_manual): {inst5=513, inst6=612}
price book (dynamic_manual): {inst5=513, inst6=614}
price book (dynamic_manual): {inst6=614, inst7=715}
price book (dynamic_manual): {inst6=616, inst7=715}
price book (dynamic_manual): {inst6=616, inst7=717}
price book (dynamic_manual): {inst7=717, inst8=818}
price book (dynamic_manual): {inst7=719, inst8=818}
price book (dynamic_manual): {inst7=719, inst8=820}
```

Identical to `dynamic_group`, state for state — which is the claim this example
makes, and what its test asserts.

```bash
cargo run -p wingfoil --example dynamic_manual --features dynamic-graph
```

Reach for this when the membership rule does not fit `dynamic_group`'s
add-stream/del-stream shape — a key retired on a timer, say, or a sub-graph
whose factory needs state the group does not thread through. Otherwise prefer
`dynamic_group`: it is the same machinery with the bookkeeping already written.
