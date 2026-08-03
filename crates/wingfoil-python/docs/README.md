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

The root `.readthedocs.yaml` still points at `legacy/wingfoil-python/docs/conf.py` —
the legacy tree keeps shipping until the cutover, and repointing it early would
break the published build. Repointing it at this directory is cutover-time work
(prerequisite row 5.5 in `docs/cutover-plan.md`), and needs three changes
together:

- `python.install[0].path` → `crates/wingfoil-python`
- `python.install[1].requirements` → `crates/wingfoil-python/docs/requirements.txt`
- `sphinx.configuration` → `crates/wingfoil-python/docs/conf.py`

Note that the pip install builds the extension from source with the full wheel
feature set (kafka, zmq, etcd, …), so the RTD build needs `protobuf-compiler` —
already in the config's `apt_packages` — and a build long enough to compile
librdkafka and libzmq.
