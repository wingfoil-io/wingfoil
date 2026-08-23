This folder contains the files Sphinx uses to generate the `wingfoil`
Python module documentation. It mirrors the legacy `legacy/wingfoil-python/docs/`
layout, so the cutover is a directory promotion rather than a re-organisation.

## Building locally

Autodoc imports the **compiled** extension, so build it before building the
docs:

```bash
cd crates/wingfoil-python
maturin develop            # or: maturin develop -F aeron,iceoryx2 for those two
cd docs
pip install -r requirements.txt
make clean
make html
```

`conf.py` fails fast with an actionable message if `wingfoil` is not
importable, rather than emitting a wall of autodoc import errors.

Adapter bindings are cargo-feature-gated, so the generated reference reflects
the features the extension was **built with**. The hand-written index in
`api.rst` covers the full surface regardless, so a partial build renders a
smaller generated table, not an error.

## Pages

| File | Contents |
|---|---|
| `index.rst`     | landing page and toctrees |
| `readme.rst`    | includes the crate `README.md` as the User Guide |
| `migration.rst` | migrating from the legacy `wingfoil` package |
| `api.rst`       | the API reference: core types, bursts, run modes, latency, adapters, the plugin seam |

## Read the Docs

The root `.readthedocs.yaml` builds **this** directory — repointed off
`legacy/wingfoil-python/docs/` in cutover-plan row 5.5. All three keys moved
together, which is what that row was about; changing one alone builds one
tree's docs against the other's module:

- `python.install[0].path` → `crates/wingfoil-python`
- `python.install[1].requirements` → `crates/wingfoil-python/docs/requirements.txt`
- `sphinx.configuration` → `crates/wingfoil-python/docs/conf.py`

Note that the pip install builds the extension from source with the full wheel
feature set (kafka, zmq, etcd, …), so the RTD build needs `protobuf-compiler` —
already in the config's `apt_packages` — and a build long enough to compile
librdkafka and libzmq.
