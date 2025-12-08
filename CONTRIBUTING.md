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
cargo build