# GitHub Actions security review

A review of all 28 workflows under `.github/workflows/`, conducted 2026-08-18
against `main`. It covers the workflow definitions themselves — triggers,
token scope, secret handling, and the supply chain the runners pull in — not
the Rust code they build.

**The two structural findings first, because they set the shape of everything
below.** What is *not* here is as important as what is: there is no
`pull_request_target`, no `issue_comment` or `workflow_run` trigger, and no
`${{ github.event.* }}` field carrying attacker-controlled text into a `run:`
block anywhere in the tree. That is the entire classic
"public-repo-CI-takeover" class, and this repository is clean of it. Publishing
runs on OIDC trusted publishing for both npm and PyPI, and AWS is reached by
role assumption rather than static keys, so three of the four registries hold
no long-lived credential at all.

What remains is almost entirely **supply chain and blast radius**: the runner
is trusted to fetch a great deal of mutable third-party code, and the secrets
are handed out much more widely than any one job needs.

Open work belongs in issues, not here — this page is the analysis, and the
fixes should be filed separately.

## The headline: the actions supply chain contradicts our own stated policy

[`SECURITY.md`](../../SECURITY.md) turns Dependabot **version** updates off on
an explicit argument, worth quoting because it is correct:

> staying at the tip of every dependency shortens the distance to a future
> security fix, but it also puts this repository in the first wave to install
> any newly published release, which is exactly the population a
> compromised-maintainer attack targets. Note that neither `cargo audit` nor
> `pnpm audit` defends against that — they match against advisory databases,
> and a freshly malicious release has no advisory yet by construction.

Every word of that applies to GitHub Actions, and **not one of the 24 distinct
action references in this tree — first-party or third-party — is pinned to a
commit SHA.** They are all mutable tags:

| Reference | Used in | What it can reach |
|---|---|---|
| `actions/checkout@v7` (×36), `Swatinem/rust-cache@v2` (×21), `dtolnay/rust-toolchain@stable` (×23) | everywhere | everything |
| `pypa/gh-action-pypi-publish@v1.14.2` | `pypi-publish.yml` | PyPI OIDC identity |
| `pulumi/actions@v5`, `aws-actions/configure-aws-credentials@v4`, `aws-actions/amazon-ecr-login@v2`, `hashicorp/setup-packer@v3.4.0`, `docker/build-push-action@v7` | the trading_e2e deploy/build workflows | the AWS role, `PULUMI_ACCESS_TOKEN`, LMAX FIX credentials |
| `PyO3/maturin-action@v1`, `taiki-e/cache-cargo-install-action@v2`, `arduino/setup-protoc@v3`, `pnpm/action-setup@v4` | build legs | the workspace, the wheels |
| `ad-m/github-push-action@v0.6.0`, `actions-rs/toolchain@v1` | `rust-fmt.yml`, `python-test.yml` | `WF_REBASE_PAT` |
| `taiki-e/install-action@nextest` | `rust-test.yml` | build tree |

`v4` and `stable` are branches-in-disguise: the owner can repoint them at any
commit, and every run picks it up on the next scheduling with no cooldown, no
advisory, and nothing in this repository to notice. `@nextest` is the same
thing without even the pretence of a version. A compromise of any one of the
accounts above is a compromise of whichever secret its workflow holds.

Two of those references are additionally to **unmaintained** actions:
`actions-rs/toolchain` has been archived by its authors for years (it also runs
on an end-of-life Node runtime), and `ad-m/github-push-action@v0.6.0` is a
similarly stale pin. An abandoned
action is the easiest kind to take over. `dtolnay/rust-toolchain` is already
used in 23 places and is the drop-in replacement for the former; the latter is
replaceable by three lines of `git push`.

**Fix:** pin every third-party action to a full commit SHA with the version in
a trailing comment, and let Dependabot's `github-actions` ecosystem propose the
moves (that ecosystem is exempt from the argument above — a SHA bump is a diff
you read, not a tip-of-tree install). First-party `actions/*` and
`github/codeql-action/*` are a judgement call; pinning them too costs nothing.

## Findings

### High

**H1 — `curl | sh` installs wasm-pack inside the npm publish job.**
`npm-publish.yml:34` runs

```sh
curl https://rustwasm.github.io/wasm-pack/installer/init.sh -sSf | sh
```

with no version, no checksum, and no signature, and then uses the result to
build the exact bundle that is published to npm **with provenance attestation**
a few steps later. The same line appears in `web-integration.yml:135`,
`web-integration.yml:169`, `build-trading-e2e-ami.yml:92` and
`build-trading-e2e-images.yml:67`. Provenance attests to *where* a build ran,
not to what it built: a bad installer here yields a signed, attested, malicious
`@wingfoil/client`. Replace with a pinned release plus checksum verification,
`cargo-binstall`, or `taiki-e/install-action` (SHA-pinned) — which is already
the mechanism used for `nextest` and `cargo-audit`.

**H2 — the release PAT is left in `.git/config` while arbitrary build scripts
run.** `actions/checkout` defaults to `persist-credentials: true`, and it is
not set to `false` in any of the 36 checkouts here. Three of them check out
with `secrets.WF_REBASE_PAT` rather than `GITHUB_TOKEN`:

- `rust-fmt.yml:15` — then runs `cargo build --verbose`, which executes
  `build.rs` for the whole dependency graph (hundreds of crates) with a
  long-lived PAT sitting readable at `.git/config`. This is the sharpest
  instance in the tree: it is precisely the "compromised transitive
  dependency" scenario `SECURITY.md` reasons about, with a repo-write
  credential in reach.
- `bump.yml:27` — same exposure across `cargo set-version` and `npm version`
  (which runs npm lifecycle scripts).
- `bulk-rebase.yml:13`.

A classic PAT cannot be scoped to a single repository, so it is also the
broadest credential in the account. **Fix:** `persist-credentials: false` on
every checkout as the default posture; where a push is needed, pass the token
to that one step via `env:` and use a fine-grained PAT (or a GitHub App
installation token) scoped to `contents: write` on this repository only.

### Medium

**M1 — 26 of 28 workflows declare no `permissions:` block.** Only
`security-audit.yml` and `pypi-publish.yml` set one at the top level; a handful
of jobs set one per-job. Everything else inherits the repository/organisation
default for `GITHUB_TOKEN`. **This needs verifying in settings** — if that
default is still the legacy *read and write*, then every integration-test job
(each of which compiles the full dependency graph and runs third-party
containers) carries a token that can push to `main`, publish packages, and
write issues. Set `permissions: contents: read` at the top of every workflow
and widen per-job where actually needed (`rust-test.yml`'s lint job already
does this correctly for `security-events: write`). Also flip the repository
default to read-only, so a new workflow starts safe.

**M2 — `secrets: inherit` hands the whole secret set to 15 workflows that
mostly need none.** `all-tests.yml`, `integration-tests.yml` and `release.yml`
each fan out with `secrets: inherit`, which passes *every* repository secret —
`CRATES_IO_API_TOKEN`, `WF_REBASE_PAT`, `AWS_ROLE_TO_ASSUME`,
`PULUMI_ACCESS_TOKEN`, `LMAX_USERNAME`/`LMAX_PASSWORD` — into all thirteen
integration workflows. Exactly one of them (`kdb-integration.yml`) uses a
secret at all. This does not by itself grant an outside attacker anything, but
it makes every one of those workflow files a place where a single added line
exfiltrates the deploy credentials, and it removes the audit signal that comes
from a secret being named where it is used. **Fix:** replace with an explicit
map — `secrets: {KDB_LICENSE_B64: ${{ secrets.KDB_LICENSE_B64 }}}` for the kdb
leg, and nothing at all for the other twelve.

**M3 — the npm publish job restores a build cache that two other workflows
write.** `npm-publish.yml:31` restores `Swatinem/rust-cache` under
`shared-key: wasm-pub`, and so do `build-trading-e2e-ami.yml:88` and
`build-trading-e2e-images.yml:63` — none of the three sets `save-if`, so all
three *write* it too. A `target/` directory is executable build state; a
poisoned entry becomes the published wasm. GitHub's cache scoping keeps a PR
branch from writing what `main` reads, so this is not reachable from a fork,
but it does mean the integrity of a signed npm artifact rests on the two AMI
workflows. The same shape applies to `cache-from/to: type=gha` feeding
`docker/build-push-action` in `build-trading-e2e-images.yml:119`. **Fix:** a
publish job should not restore a shared mutable cache. Drop the cache step from
`npm-publish.yml`, or give it a dedicated key with `save-if: false`. The main
CI legs already get this right (`rust-test.yml` restricts saving to
`refs/heads/main`); the integration legs and `wasm-pub` do not.

**M4 — no container image is pinned by digest, and one is `:latest` from a
personal namespace.** `aeron-integration.yml` pulls
`neomantra/aeron-cpp-debian:latest` and runs it with `-v /dev/shm:/dev/shm`.
The rest (`gcr.io/etcd-development/etcd:v3.5.0`, `redpanda:v24.1.1`,
`redis:7-alpine`, `postgres:16-alpine`, `otel/opentelemetry-collector:0.149.0`,
`infinyon/fluvio:0.18.1`) at least carry a version, but a tag is still mutable.
Pin by `@sha256:…`, and treat the `:latest` one as the priority.

**M5 — CI gates can be skipped by choosing a commit message.** The `bump:`
guard in `rust-test.yml:62`, `python-test.yml:77` and `web-integration.yml:42`
tests nothing but the text of `github.event.head_commit.message`:

```yaml
!(startsWith(github.event.head_commit.message, 'bump: ')
  && contains(github.event.head_commit.message, ' version to '))
```

Any push to `main` whose head commit message matches that shape skips the Rust
tests, clippy, rustfmt, the example-docs check, the Python interop leg and all
three web legs. A squash merge lets whoever presses the button set that
message. The intent is sound and documented, and pull requests are unaffected
(no `head_commit`), so this is an integrity gap rather than an access one —
but it is worth ANDing the guard with something the bot alone controls
(`github.actor`, or a marker `release.bump` writes into a file the guard
reads) rather than leaving the CI gate keyed on free text.

**M6 — secrets are spliced into shell scripts by expression interpolation.**
`${{ secrets.X }}` inside a `run:` block is textual substitution performed
*before* the shell parses the script, so the secret's bytes become part of the
program. Present in `kdb-integration.yml` (`echo "${{ secrets.KDB_LICENSE_B64
}}" | base64 -d > /tmp/kc.lic`) and `deploy-trading-e2e.yml` (the two
`pulumi config set --secret lmax_*` lines and the five `*_IMAGE` lines). The
current values are benign — base64 and image URIs — so nothing is broken today,
but the pattern only holds while that stays true. **Fix:** pass through `env:`
and reference `"$VAR"`, which never re-enters the parser. While there:
`/tmp/kc.lic` is written world-readable before being bind-mounted; `install
-m 600` it.

### Low

**L1 — free-form dispatch inputs reach `$GITHUB_OUTPUT`.**
`build-trading-e2e-ami.yml` takes `image_tag` and `instance_type` as unvalidated
strings (every other dispatch input in the tree is `type: choice`), and
`steps.ecr` echoes `IMAGE_TAG` into `$GITHUB_OUTPUT`. A newline in the value
injects additional step outputs — including `registry`, which decides where
Packer authenticates and what it bakes. Requires write access to exploit, so
it is defence in depth: validate against `^[A-Za-z0-9._-]+$` before use.

**L2 — dispatch inputs interpolated into `case` statements.**
`pypi-publish.yml:302`, `bump.yml:40`, `deploy-trading-e2e.yml:70`. All are
`type: choice` today except `pypi-target` on the `workflow_call` path, whose
only caller passes a literal. Same fix as M6: read them from `env:`.

**L3 — public run summaries leak account detail.** The repository is public, so
`$GITHUB_STEP_SUMMARY` is world-readable. `build-trading-e2e-images.yml:127`
prints the full ECR hostname, which contains the **AWS account ID**;
`deploy-trading-e2e.yml:221` dumps `pulumi stack output --json` wholesale
(Pulumi masks values marked secret, but not hostnames, ARNs, IPs or anything
added later without the marker). Neither is a credential; both are
reconnaissance, and both are easy to trim.

**L4 — `bulk-rebase.yml` force-pushes every branch in the repository.** It
loops over all remote refs, rebases each onto `main`, and force-pushes with a
PAT. `--force-with-lease` is used, which is the right instinct, but the lease is
against refs fetched at job start, so it protects against nothing that happens
after. There is no allowlist, no dry-run and no exclusion for release or
long-lived branches. `$branch` is also unquoted (`git checkout $branch`) —
harmless in practice, since git ref names forbid the characters that would
matter, but it should be quoted anyway.

**L5 — integration workflows trigger on `push` with no `branches:` filter,**
so they run on every branch pushed to the base repository rather than just
`main`. Combined with M2 that is thirteen extra runs per branch push, each
holding the full secret set. Add `branches: [main]` (the reusable
`workflow_call` path is how they run for PRs anyway).

## What is already right

Worth recording, so none of it gets traded away in a later cleanup:

- **No `pull_request_target`, anywhere.** This is the single most important
  property in the whole review.
- **OIDC trusted publishing** for both npm and PyPI, with `id-token: write`
  granted per-job and correctly re-granted at the caller in `release.yml`
  (reusable-workflow permissions are capped by the caller). No long-lived
  registry tokens for either. AWS likewise uses role assumption rather than
  static access keys. `CRATES_IO_API_TOKEN` is now the only registry secret
  left — crates.io trusted publishing exists and would close that gap too.
- **The ECR password is masked** (`::add-mask::`) before being handed to
  Packer — `build-trading-e2e-ami.yml:162`.
- **Lockfiles are enforced** (`pnpm install --frozen-lockfile`), pnpm 10 blocks
  install lifecycle scripts by default, and `security-audit.yml` runs
  `cargo audit`, `pnpm audit` and `dependency-review` on a schedule as well as
  per-PR.
- **Every job has a `timeout-minutes`.** Rare, and it caps runaway spend.
- `rust-test.yml` restricts cache *writes* to `refs/heads/main` — the correct
  pattern, which just needs extending to the other shared keys (M3).
- ECR repositories are created with `scanOnPush=true`.

## Suggested order

1. Pin every third-party action to a SHA, and drop the two unmaintained ones
   (the headline section) — largest reduction in attack surface per line
   changed.
2. `persist-credentials: false` everywhere; scope or replace `WF_REBASE_PAT`
   (H2).
3. Replace the `curl | sh` wasm-pack installs (H1).
4. Explicit `permissions:` on every workflow, and flip the repository default
   to read-only (M1).
5. Replace `secrets: inherit` with explicit maps (M2).
6. Everything else, in severity order.

Steps 1 and 4 are mechanical and touch every file; doing them first means the
rest lands on a tree where a mistake is already contained.
