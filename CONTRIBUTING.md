
## We're looking for contributors!

Hi! Thanks for your interest in contributing to **wingfoil** — we'd love to have your participation! 

Drop a comment on any issue, open a new one, or say hi on [Discord](https://discord.gg/WfZwpQnZUA), email `hello@wingfoil.io`

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

## Good First Issues

New to open source or Rust? These are a great starting point:

- 🧮 [Add EWMA stream](https://github.com/wingfoil-io/wingfoil/issues/111)
- 🔍 [Python binding for inspect & throttle](https://github.com/wingfoil-io/wingfoil/issues/112)


## Building and Testing

### Prerequisites

These tools are required for building, testing, and packaging the core **wingfoil** project:

* **The Rust toolchain:** `rustup`, `cargo`, `rustc`, etc. We aim for compatibility with the latest stable version.
* **`rustfmt` and `clippy`:** We use `rustfmt` for consistent code style and `clippy` for linting across the whole code base.
* **`protoc` (Protocol Buffers compiler):** required when building with `--all-features` (used transitively by `etcd-client` and a few other adapters). The easiest way to get it (Linux/macOS) is:

  ```bash
  ./scripts/setup-dev.sh
  ```

  Or install manually — Debian/Ubuntu: `sudo apt-get install -y protobuf-compiler`; macOS: `brew install protobuf`.

For prerequisites specific to the **wingfoil-python** crate and the full build process, please see the [**BUILD.md**](https://github.com/wingfoil-io/wingfoil/blob/main/wingfoil-python/build.md) documentation.

#### Aeron adapter

The Aeron adapter requires clang, libuuid, and a recent CMake (the version in apt is often too old):

```bash
sudo apt update
sudo apt install clang libclang-dev uuid-dev

wget https://github.com/Kitware/CMake/releases/download/v3.31.0/cmake-3.31.0-linux-x86_64.sh
sudo ./cmake-3.31.0-linux-x86_64.sh --prefix=/usr/local --skip-license
```

### Building

```bash
cargo build                    # default features
cargo build --features full    # everything CI builds (needs protoc)
```

### Pre-PR check (matches CI)

CI is configured in [`.github/workflows/rust.yml`](.github/workflows/rust.yml). The same checks are wrapped as cargo aliases in `.cargo/config.toml` so you can run them locally with one command each:

```bash
cargo fmt --all -- --check     # formatting
cargo lint                     # clippy, default features
cargo lint-all                 # clippy, all features  ← most-missed step
cargo test -p wingfoil --features full
```

`cargo lint-all` is the step that most often surfaces issues that pass locally but fail in CI — it exercises code behind feature flags (`fix`, `csv`, `iceoryx2-beta`, `kdb`, etc.) that the default build skips. Please run it before pushing.






