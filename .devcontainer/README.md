# Devcontainer

A container with everything a first contribution needs: current stable Rust
with `rustfmt` and `clippy`, `protoc`, Python 3.11 with `maturin` and `pytest`,
and a Docker daemon for the adapter integration tests.

## Two ways in

**GitHub Codespaces** — *Code → Codespaces → Create codespace on main*. Nothing
to install locally.

**Locally** — Docker plus the [Dev
Containers](https://marketplace.visualstudio.com/items?itemName=ms-vscode-remote.remote-containers)
extension, then *Dev Containers: Reopen in Container*. Any devcontainer-aware
editor works; the config is not VS Code-specific beyond the `customizations`
block.

Either way, when it finishes:

```bash
cargo test -p wingfoil
cargo run  -p wingfoil --example hello_graph
```

## What is in it, and why

| | |
|---|---|
| `mcr.microsoft.com/devcontainers/rust:1-bookworm` | Rust, cargo, rustfmt, clippy |
| `post-create.sh` → `rustup update stable` | The image pins the stable of its build date; CI runs today's stable, and newer clippy emits lints the older one does not. That gap is documented in `CLAUDE.md` and this closes it |
| `post-create.sh` → `scripts/setup-dev.sh` | `protoc`, which a plain workspace build needs — a transitive dependency compiles proto files |
| `post-create.sh` → `cargo fetch --locked` | So the first build is compile-bound, not network-bound |
| Python 3.11 feature + a venv in `crates/wingfoil-python/.venv` | Same layout `python-test.yml` uses, so the local and CI commands match |
| docker-in-docker feature | `tests/<name>_integration.rs` for etcd, redis, postgres, kafka, fluvio, otlp and aeron start their own containers through `testcontainers` |

**It does not build the tree.** A `--all-targets` dev build is ~9.2GB and
several minutes, and most first contributions touch one crate. If you want that
warmed up, a [Codespaces
prebuild](https://docs.github.com/en/codespaces/prebuilding-your-codespaces)
is the place for it — add a `cargo build -p wingfoil --all-targets` step there
rather than to `post-create.sh`, so everyone else keeps a fast create.

## Disk

`hostRequirements` asks for 64GB, and that is not padding: `cargo lint` and
`cargo lint-all` cannot share artifacts (different feature sets hash to
different metadata), incremental adds ~2.6GB, and a default 32GB codespace runs
out partway through the second lint. On "no space left on device":

```bash
scripts/disk.sh          # what is using it
scripts/disk.sh light    # drop examples/benches/incremental, keep deps/
```

`light` keeps `target/*/deps`, so the next build relinks instead of
recompiling 700+ crates. Deletes still succeed while writes are failing, so
the container is recoverable — you do not need a fresh one.

## Not what the maintainers use

This is a convenience for contributors, not a pinned build environment. CI in
`.github/workflows/` is the authority on what a green build is; if the two ever
disagree, CI is right.
