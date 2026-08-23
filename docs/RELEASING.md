# Releasing

How a wingfoil release is cut, what has to be true before you start, and what
to do when a publish fails halfway. Maintainer-facing; the workflows in
[`.github/workflows/`](../.github/workflows/) are the source of truth and this
page explains them.

A release ships **one version to three registries** — crates.io, PyPI and npm —
and the tag is cut only once all three have the artifacts.

## Before the first release from a fresh setup

Two things are configured outside this repository and are easy to forget,
because nothing in the tree fails without them until a release run does:

1. **Trusted publishers must be registered on both PyPI *and* TestPyPI.**
   `pypi-publish.yml` uploads over OIDC — there is no `*_PYPI_API_TOKEN`
   secret. Register a GitHub publisher on each of the two indexes for the
   project `wingfoil`, owner `wingfoil-io`, repository `wingfoil`, workflow
   `pypi-publish.yml`, and **no environment** (leave the environment field
   blank — it must match the workflow, and `upload-pypi` declares no
   `environment:`). TestPyPI matters as much as PyPI: without it the rehearsal
   in step 2 fails at its very last step, after the whole wheel matrix has
   built.
2. **Dispatch `pypi-publish.yml` once with `pypi-target: test`.** That is the
   only way to exercise the whole PyPI path — sdist repair, five wheels, the
   `upload-pypi` job's OIDC mint — without burning a version number on the real
   index. `test` is the default for a manual dispatch, so this is also what you
   get if you dispatch that workflow without thinking; only `release.yml` passes
   `prod`. Do it after any change to that workflow, not just once ever.

npm and crates.io are already set up: npm publishes over OIDC too (trusted
publisher for `wingfoil-io/wingfoil` + `npm-publish.yml` on `@wingfoil/client`),
and crates.io uses the `CRATES_IO_API_TOKEN` secret.

## The release, end to end

Two manual dispatches, in this order.

### 1. `release.bump`

Dispatch [`bump.yml`](../.github/workflows/bump.yml) with `major` / `minor` /
`patch`. It computes the next version from the highest of the three version
strings it tracks, then moves them all together and pushes the commit **straight
to `main`**:

- every root-workspace crate (`cargo set-version`);
- `crates/wingfoil-wasm/Cargo.toml`, which is excluded from the workspace and so
  is bumped by `sed`, along with its `wingfoil-wire-types` pin;
- `js/package.json` (`@wingfoil/client`);
- `crates/wingfoil-python/docs/conf.py`;
- the `esm.sh/@wingfoil/client@<version>` importmap pins in the `trading_e2e`
  examples.

The commit message is `bump: <type> version to <x.y.z>`, and that exact shape is
what the heavy CI legs skip on (see
[`.github/workflows/README.md`](../.github/workflows/README.md)).

Write the [release-notes page](release-notes/) for the new version before step 2
if the release needs one — `github-release` links `docs/release-notes/<version>.md`
when it exists and the index otherwise, so a page added later is not linked from
the release body.

### 2. `release`

Dispatch [`release.yml`](../.github/workflows/release.yml). One run, in this
order:

```
preflight ─> all tests ─┬─> crates.io ─┐
                        ├─> npm        ├─> tag ─> GitHub release
                        └─> PyPI       ┘
```

**Preflight** reads the version from `crates/wingfoil/Cargo.toml` and fails if
`js/package.json` or `wingfoil-wasm` disagree with it, or if the tag already
exists.

**All tests** is `all-tests.yml`: `rust-test.yml` + `python-test.yml` + the
whole `integration-tests.yml` fan-out.

**The three publishes run in parallel**, each a reusable workflow:

- `crates-publish.yml` — six crates to crates.io in dependency order, with
  `sleep 30` waits for the index between tiers: `wingfoil-derive`,
  `wingfoil-python-derive`, `wingfoil-wire-types` → `wingfoil`, `wingfoil-wasm`
  → `wingfoil-python`. `wingfoil-wasm` is verified against
  `wasm32-unknown-unknown` (the target is installed in the job for exactly
  that), and every step passes `--registry crates-io` because these crates list
  two registries in `package.publish`.
- `npm-publish.yml` — builds the wasm bundle and the TypeScript, lints, runs
  `vitest` as a publish preflight, checks the pack tarball actually contains
  `dist/wasm/wingfoil_wasm_bg.wasm`, then `npm publish --access public` over
  OIDC (provenance is emitted automatically). It publishes with the **npm**
  CLI, not pnpm, which has no OIDC trusted publishing — and pins npm to the
  11.x line, because 12.0.0's bundle cannot resolve `sigstore`.
- `pypi-publish.yml` with `pypi-target: prod` — one sdist plus five abi3 wheels
  (linux x86_64 + aarch64 in `manylinux_2_28` containers, macOS arm64 + x86_64,
  windows x64), then one `upload-pypi` job over OIDC. The Linux wheels carry
  every adapter (`--features extension-module,all-adapters`); macOS and Windows
  take the pyproject feature list, which excludes aeron and iceoryx2. The upload
  job refuses to publish fewer than 5 wheels or anything other than exactly 1
  sdist.

`release.yml` grants `id-token: write` on the `publish-npm` and `publish-pypi`
jobs itself. A reusable workflow's permissions are capped by its caller, so the
grant has to be at the call site or the upload job cannot mint an OIDC token.

**Then the tag**, and only then. This ordering is the fix for a real failure
mode: the tag used to come first, so a publish failure left a pushed tag that
made preflight's "tag already exists" check block every re-run, and the only way
forward was deleting a tag by hand. Nothing consumes the tag — each publish job
checks out the dispatched commit, not the tag — so moving it last costs nothing.

**Finally the GitHub release**, which needs the tag to exist
(`gh release create --verify-tag`). Its body is an install block for all three
registries, a link to the release notes, and GitHub's generated changelog
appended underneath.

## When a publish fails

The recoverable-but-not-idempotent case is a **partial** publish. If crates.io
succeeds and npm fails, re-dispatching `release.yml` re-runs the crates job,
which dies on *crate version already uploaded*.

That state is still clean in the ways that matter — **no tag was pushed and no
GitHub release exists**, so there is nothing to hand-delete and the version is
not yet announced. The way out is to dispatch the *remaining* registries
individually rather than re-running `release.yml`:

- `crates-publish.yml`, `npm-publish.yml`, `pypi-publish.yml` are each
  dispatchable on their own. That is what they are for outside a release run.
- Then push the tag by hand and create the release, or re-dispatch
  `release.yml` only once every registry is done — its publish jobs will still
  fail on "already uploaded", so hand-tagging is usually the shorter path.

None of the three registries lets you overwrite a published version, so a bad
release is fixed by bumping again, not by republishing.

## Related

| | |
|---|---|
| [`.github/workflows/README.md`](../.github/workflows/README.md) | Every workflow in the repo, what triggers it, and the CI/integration split |
| [`release-notes/`](release-notes/) | The curated half of a release — one page per version |
| [`../SECURITY.md`](../SECURITY.md) | Dependency policy, and why Dependabot version updates are off |
