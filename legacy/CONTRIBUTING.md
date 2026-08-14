
## We're looking for contributors!

Hi! Thanks for your interest in contributing to **wingfoil** — we'd love to have your participation! 

Drop a comment on any issue, open a new one, or say hi on [Discord](https://discord.gg/WfZwpQnZUA), email `hello@wingfoil.io`

We're actively looking for help on the following:

- 📦 [Binary file I/O](https://github.com/wingfoil-io/wingfoil/issues/104) — Arrow, Parquet, and more
- 🐍 [wingfoil-python full parity](https://github.com/wingfoil-io/wingfoil/issues/106) — every node and adapter exposed to Python
- 🐍 [Python showcase](https://github.com/wingfoil-io/wingfoil/issues/107) — Rust pipeline, results in pandas + scikit-learn + plotly

Shipped since this list was written, and no longer open:
[ZMQ service discovery](https://github.com/wingfoil-io/wingfoil/issues/103),
[KDB+ caching](https://github.com/wingfoil-io/wingfoil/issues/90),
[SQL I/O](https://github.com/wingfoil-io/wingfoil/issues/105) (the postgres
adapter), [Kafka I/O](https://github.com/wingfoil-io/wingfoil/issues/23) and
[JS/TS browser integration](https://github.com/wingfoil-io/wingfoil/issues/110)
(`wingfoil-wasm` + `@wingfoil/client`).

We're especially keen to hear from specialists in:

- 🔌 FPGA / rusthdl
- 🌐 WASM / JS / TS
- 🐍 PyO3

## Good First Issues

New to open source or Rust? Browse the
[`good first issue`](https://github.com/wingfoil-io/wingfoil/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
label, or anything labelled `size: small`. **New work belongs on the wingfoil
tree at the repository root, not here** — see the root
[`CONTRIBUTING.md`](../CONTRIBUTING.md). This tree is frozen at 8.0.0 and is
deleted at cutover.


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

CI is configured in [`.github/workflows/rust-test.yml`](../.github/workflows/rust-test.yml). The same checks are wrapped as cargo aliases in `.cargo/config.toml` so you can run them locally with one command each:

```bash
cargo fmt --manifest-path legacy/Cargo.toml --all -- --check   # formatting
cargo lint-legacy              # clippy, default features
cargo test-legacy              # the whole legacy workspace
cargo test --manifest-path legacy/wingfoil/Cargo.toml --features full
```

**Every command needs the manifest path.** This tree is its own cargo
workspace — it left the root one ahead of the cutover rename, since
`wingfoil-next` becomes `wingfoil` and one workspace cannot hold two packages
of that name (`docs/planning/cutover-plan.md` 5.0). Plain `cargo lint` / `cargo test
-p wingfoil` from the repo root no longer sees this tree, and **the git hooks
do not cover it either** — they run `--workspace` against the root. Run the
above by hand before pushing; CI gates it in `Lint legacy` and
`Test (wingfoil) & Coverage`.

For the all-features clippy pass:

```bash
cargo clippy --manifest-path legacy/Cargo.toml --workspace --all-targets --all-features -- -D warnings
```

`cargo lint-all` is the step that most often surfaces issues that pass locally but fail in CI — it exercises code behind feature flags (`fix`, `csv`, `iceoryx2`, `kdb`, etc.) that the default build skips. Please run it before pushing.






