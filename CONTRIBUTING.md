Hi! Thanks for your interest in contributing to **wingfoil** — we'd love to have your participation! 

If you want help or mentorship, please do reach out: you can raise a **GitHub issue** or email `hello@wingfoil.io`

---

## Building and Testing

### Prerequisites

These tools are required for building, testing, and packaging the core **wingfoil** project:

* **The Rust toolchain:** `rustup`, `cargo`, `rustc`, etc. We aim for compatibility with the latest stable version.
* **`rustfmt` and `clippy`:** We use `rustfmt` for consistent code style and `clippy` for linting across the whole code base.

For prerequisites specific to the **wingfoil-python** crate and the full build process, please see the [**BUILD.md**](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil-python/build.md) documentation.

### Building

```bash
cargo build```

---

## Good First Issues

New to open source or Rust? These are a great starting point:

- 🧮 [Add EWMA stream](https://github.com/wingfoil-io/wingfoil/issues/111)
- 🔍 [Python binding for inspect & throttle](https://github.com/wingfoil-io/wingfoil/issues/112)

---

## We're Looking For Contributors

We're actively looking for help on the following:

- 🔧 [ZMQ service discovery](https://github.com/wingfoil-io/wingfoil/issues/103) — dynamic node registration
- 🗄 [KDB+ caching](https://github.com/wingfoil-io/wingfoil/issues/90) — faster replay and snapshot support
- 📦 [Binary file I/O](https://github.com/wingfoil-io/wingfoil/issues/104) — Arrow, Parquet, and more
- 🛢 [SQL I/O](https://github.com/wingfoil-io/wingfoil/issues/105) — stream to/from relational databases
- ⚡ [Kafka I/O](https://github.com/wingfoil-io/wingfoil/issues/23) — streaming integration
- 🐍 [wingfoil-python full parity](https://github.com/wingfoil-io/wingfoil/issues/106) — every node and adapter exposed to Python
- 🐍 [Python showcase](https://github.com/wingfoil-io/wingfoil/issues/107) — Rust pipeline, results in pandas + scikit-learn + plotly
- 🌐 [JS/TS browser integration](https://github.com/wingfoil-io/wingfoil/issues/110) — wingfoil in-browser via WASM

We're especially keen to hear from specialists in:

- 🔌 FPGA / rusthdl
- 🌐 WASM / JS / TS
- 🐍 PyO3

Drop a comment on any issue, open a new one, or say hi on [Discord](https://discord.gg/WfZwpQnZUA).
