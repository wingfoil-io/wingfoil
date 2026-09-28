# cache-secret-audit

A GitHub Action that finds workflow layouts where a secret can end up in a
build cache a pull request can read. In September 2026 the Rust project
disclosed that `cargo miri` wrote the full process environment into
`target/`, so any secret in a Miri job's environment ended up in the
Actions cache, and any PR that restored that cache could read it
([advisory](https://blog.rust-lang.org/2026/09/21/github-actions-leaking-secrets-when-miri-output-is-cached/)).
The Miri bug is fixed, but the layout that made it exploitable is still
common: a secret in job-level `env:`, `cargo` running while that secret is in
scope, and a `target/` cache that PR runs can read or write. Any build
script, proc macro or test can write to `target/` the same way Miri did.
This action flags that layout. It is one Python file with PyYAML as its only
dependency.

## Usage

```yaml
jobs:
  cache-secret-audit:
    runs-on: ubuntu-latest
    permissions:
      contents: read
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7
        with:
          persist-credentials: false
      - uses: wingfoil-io/wingfoil/actions/cache-secret-audit@<SHA> # cache-secret-audit-v0.1.0
        with:
          path: .github/workflows   # default
          min-severity: medium      # low | medium | high
          fail: "true"              # "false" reports without failing, for first adoption
```

`<SHA>` is the commit the `cache-secret-audit-v0.1.0` tag points to. The tag
has that prefix because this action lives in the wingfoil repository, where
plain `vX.Y.Z` tags are crate releases. Pin by SHA. A tag can be moved and a
SHA cannot.

You can also run it locally with `pip install pyyaml` and
`python cache_secret_audit.py [--format json] [--min-severity S] [--no-fail] [PATH ...]`.

## Rules

A workflow counts as **PR-triggered** when its `on:` includes `pull_request`
or `pull_request_target`, or `workflow_call`. A reusable workflow runs under
its caller's event, so v0.1.0 assumes a PR caller and does not resolve the
call graph. A **secret reference** is `secrets.X` or `secrets['X']` inside
`${{ }}`, and `${{ secrets.X || 'default' }}` still counts. `secrets.GITHUB_TOKEN`
and `github.token` do not count. That token is minted for each job, is scoped
by `permissions:`, and expires when the job ends, so a copy restored from a
cache is already dead.

Every finding comes with a one-paragraph `why` and a `fix` snippet. They show
up in the annotation, in `--format json`, and in the step summary.

### R1 `secret-in-job-env` (high)

A PR-triggered workflow has a secret in workflow-level or job-level `env:`.
That exports the secret to every step and every process those steps start.
Step-level `env:` is the right scope, so R1 does not flag it.

```yaml
# Fails
on: pull_request
jobs:
  test:
    env:
      MY_TOKEN: ${{ secrets.MY_TOKEN }}
    steps:
      - run: ./test.sh
```

```yaml
# Passes
on: pull_request
jobs:
  test:
    steps:
      - run: ./test.sh
      - name: Upload
        env:
          MY_TOKEN: ${{ secrets.MY_TOKEN }}
        run: ./upload.sh
```

### R2 `pr-cache-save` (medium)

A PR-triggered workflow has a cache step that can write the cache during a PR
run:

- `Swatinem/rust-cache` with no `save-if`, or with a `save-if` that does not
  limit saves to `refs/heads/main` / `refs/heads/master` or
  `event_name == 'push'`. `save-if: false` also passes.
- `actions/cache` with no `lookup-only: true` and no PR-excluding `if:`.
- `actions/cache/save` with no PR-excluding `if:`.

An `if:` excludes PRs when every `||` branch includes one of those positive
tests as an `&&` term, on the step or on its job. `event_name != 'pull_request'`
also counts, but only when `pull_request_target` is not a trigger. The check
is deliberately conservative: `|| true` and `!( … )` do not pass.

```yaml
# Fails
on: pull_request
jobs:
  test:
    steps:
      - uses: Swatinem/rust-cache@v2
      - run: cargo test
```

```yaml
# Passes
on: pull_request
jobs:
  test:
    steps:
      - uses: Swatinem/rust-cache@v2
        with:
          save-if: ${{ github.ref == 'refs/heads/main' }}
      - run: cargo test
```

### R3 `cargo-with-secret-in-scope` (high)

A step whose `run:` calls `cargo` has a secret in scope, from workflow `env:`,
job `env:`, the step's own `env:`, or written inline in the `run:` script.
R3 applies to **every trigger**. A push-only job on `main` that leaks a
secret into `target/` saves it in the cache, and the next PR restores it.

One narrow case is exempt: a registry token (`CARGO_REGISTRY_TOKEN`,
`CARGO_REGISTRIES_<NAME>_TOKEN`) in the step's own `env:`, in a step whose
only cargo commands are `publish`, `login`, `logout`, `owner` or `yank`, with
no other secret in scope. That is how cargo documents passing its token, and
Miri's fix also keeps `CARGO_*_TOKEN` out of `target/`.

```yaml
# Fails
on: push
jobs:
  test:
    steps:
      - env:
          SENTRY_DSN: ${{ secrets.SENTRY_DSN }}
        run: cargo test
```

```yaml
# Passes
on: push
jobs:
  test:
    steps:
      - run: cargo test
      - env:
          SENTRY_DSN: ${{ secrets.SENTRY_DSN }}
        run: ./notify.sh
```

### R4 `miri-with-cache` (high)

A workflow runs `cargo miri` and also has a cache step (any of the actions R2
checks, in any configuration, under any trigger). This checks directly for the
disclosed bug.

```yaml
# Fails
on: push
jobs:
  miri:
    steps:
      - uses: Swatinem/rust-cache@v2
      - run: cargo +nightly miri test
```

```yaml
# Passes
on: push
jobs:
  miri:
    steps:
      - run: rustup +nightly component add miri
      - run: cargo +nightly miri test
```

## Exit codes

| Code | Meaning |
|---|---|
| 0 | No findings at or above `--min-severity` (default `medium`), or `--no-fail` / `fail: "false"` was set |
| 1 | At least one finding at or above `--min-severity` |
| 2 | A workflow could not be parsed, or a path does not exist. `--no-fail` does not suppress this |

## What this does not do

This is **not** [zizmor](https://github.com/zizmorcore/zizmor), and it does not
replace it. zizmor covers the broad class of Actions problems: template
injection, `pull_request_target` misuse, unpinned actions, credential
persistence, cache poisoning in release workflows, and more. This action
checks one narrow thing: a secret combined with a cargo build and a cache
that PRs can reach. Run both.

v0.1.0 also does not:

- resolve reusable-workflow call graphs. `workflow_call` is simply treated as
  PR-triggered.
- recurse into composite actions.
- follow shell indirection. A `$VAR` that expands to `cargo` is not seen.

## Why only setup-python

The action has one composite step that uses `actions/setup-python`, pinned by
SHA, then installs `pyyaml==6.0.3` into a throwaway venv and runs the script.
It uses no Docker, no Node and no other third-party action. A tool that
audits your supply chain should add as little to it as possible.

## Contributing

Tests run with `pip install pyyaml pytest && pytest` from this directory.
Every file in `tests/fixtures/pass/` must produce no findings. Every file in
`tests/fixtures/fail/` must fire exactly the rules its filename names: either
a rule name prefix (`pr-cache-save-…`) or an ID list (`r1+r3-…`).
wingfoil's own CI (`.github/workflows/cache-secret-audit.yml`) also runs the
action over this repository's workflows with `min-severity: high`. R1, R3 and
R4 fail the build there, and R2 findings show up as warning annotations.

A rule that zizmor could reasonably host should go to zizmor. A zizmor issue
proposing these checks will be linked here once it is open.
