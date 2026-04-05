## Building from source

```bash
# install deps (system)
#
# - `uv` (https://astral.sh/uv) for Python toolchain + env management
# - Rust toolchain (via rustup)
# - `patchelf` (recommended on Linux for maturin rpath handling)

cd wingfoil-python

# install a Python runtime (managed by uv; no global python required)
uv python install 3.11

# create a local virtual env managed by uv
uv venv --python 3.11

# install Python dependencies from `pyproject.toml` (no pip)
uv sync --extra dev --locked

# build and install the wingfoil extension into the uv environment
uv run maturin develop --release

# run tests
uv run pytest

# run an example
uv run python ./examples/quick_start.py

```
