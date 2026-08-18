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

## Has any of this been exploited?

Asked after the review, and worth recording with the evidence rather than a
reassurance. **No — and for most findings there was never a population that
could have.** What follows is what was checked and what it showed, as of
2026-08-18.

### The access population is one person

`wingfoil-io/wingfoil` has exactly **one collaborator**: `0-jake-0`, admin.
There are no other collaborators at any permission level. Since almost every
finding above (H2, M1, M2, M5, L1, L2, L4) requires *write* access to reach,
the set of people who could have abused them is the owner. "External
contributor" and "can exploit these" have not overlapped.

### Nothing external ever touched the workflows

Across the **232 commits** that have ever modified `.github/workflows/`
(2025-09-17 → 2026-08-16):

| Author | Commits |
|---|---|
| Jake Mitchell (owner) | 210 |
| Claude | 15 |
| dependabot[bot] | 5 |
| `tommy-ca` | 1 |
| `terraplanetary` | 1 |

The two external ones were read in full. Both are ordinary merged feature PRs
that added an integration workflow for the adapter they contributed —
`tommy-ca` the iceoryx2 adapter (#176), `terraplanetary` the Kafka adapter
(#180). Neither touches credentials, triggers or permissions beyond copying
the file's existing shape. Both were merged through GitHub by the owner
(committer is `GitHub`, i.e. the merge button), not pushed directly.

### No exfiltration shape has ever been in a workflow

The complete diff history of `.github/workflows/` (34,420 lines) was scanned
for the patterns an attack leaves behind. Every one returns zero:
`pull_request_target`, `toJSON(secrets)`, `env | base64`, `printenv`,
`curl -d`/`--data`, `/dev/tcp/`, `nc -e`, `eval`, `ngrok`, `webhook.site`,
`requestbin`. **`pull_request_target` has never appeared in this repository at
all** — not once, in any commit, which is the single most reassuring fact in
this section.

Every external host any workflow has ever contacted is accounted for and
expected: `github.com`, `rustwasm.github.io`, `pypi.org`/`test.pypi.org`,
`registry.npmjs.org`, `crates.io`, `app.pulumi.com`, `dl.fedoraproject.org`,
`esm.sh`, `rust-lang.github.io`. Nothing unrecognised.

### Every credential-bearing run was started by the owner

All **281** `workflow_dispatch` runs in the repository's history
(2025-11-08 → 2026-08-14) were enumerated — full coverage, not a sample. Every
one has `actor = triggering_actor = 0-jake-0`. That covers all of
`release`, `release.bump`, `publish.npm`, `publish.pypi`, `crates.io pub`,
`trading-e2e.deploy`, `trading-e2e.build.*`, `maintenance.bulk-rebase` and
`fmt` — i.e. every path that touches `CRATES_IO_API_TOKEN`, `WF_REBASE_PAT`,
the AWS role, the Pulumi token or the LMAX credentials. No delegated run, no
`actor != triggering_actor` mismatch, no external dispatcher.

Tags are a clean sequence to `v8.0.0` with no anomalous or unexpected version.

### One real historical exposure — closed the same day, owner-only

The one finding this exercise turned up that the review above did not:
**`fix-integration.yml` briefly combined a `pull_request` trigger with the live
LMAX FIX credentials.** Introduced 2026-04-13 in `9bd9ef9` ("FIX-protocol",
#164), which put `LMAX_USERNAME`/`LMAX_PASSWORD` into a test step's `env:`
while the workflow also fired on `pull_request`; removed by the next commit to
that file **the same day**. The secrets themselves left the file on 2026-05-03.

Both `pull_request` runs of that workflow have been inspected. Both are from
2026-04-13, both have `actor = 0-jake-0`, and both have
`head_repository = wingfoil-io/wingfoil` — same-repo branch `FIX-protocol`, not
a fork. So the credentials were available only to the owner's own pull request.
This would have mattered had it survived: a fork PR cannot read secrets, but a
`pull_request` trigger plus a real secret is one policy change away from being
serious, and trading credentials are the worst thing in the store to have on
that path. It was open for hours, to an audience of one.

`CODECOV_TOKEN` sat on a `pull_request`-triggered workflow far longer
(2025-12-06 → 2026-08-14, `rust.yml` then `rust-test.yml`), but it was only
ever passed to `codecov/codecov-action` with `continue-on-error: true` and
`fail_ci_if_error: false`, it is a low-value upload token, and GitHub supplies
no secrets to fork PRs — which is directly corroborated by `tommy-ca`'s own
commit trail on #176 ("ci(python): skip codecov uploads without token", "gate
codecov uploads on token"): the external contributor was patching around a
token that was empty for them, which is the control working.

That same commit trail ("docs: record upstream workflow approval needed",
"docs: record fork CI + upstream `action_required`") also confirms GitHub's
first-time-contributor **workflow approval gate** was in effect for fork PRs —
their runs sat in `action_required` until a maintainer released them.

### What this could not check

Stated so the conclusion is not read wider than the evidence:

- **The organisation audit log** (secret access, settings changes, PAT
  creation, permission grants) needs the admin audit-log API and was not
  reachable from here. It is the one source that would show credential *use*
  rather than credential *opportunity*.
- **Step logs**, which GitHub retains for ~90 days. Run *metadata* going back
  to 2025-11 was checked; the logs behind older runs are gone.
- **Whether published artifacts match their tagged source.** No byte-level
  diff of the crates.io/PyPI/npm artifacts against the tree was performed.
- **Cache contents**, which are opaque via the API.

### Two things to do anyway

Independent of the finding that nothing was abused:

1. **Rotate `LMAX_USERNAME` / `LMAX_PASSWORD`.** They spent a window, however
   short and however owner-only, on a `pull_request`-triggered workflow, and
   they are the highest-consequence secret in the store. Rotation is cheap;
   the counterfactual is not.
2. **Delete the retired secrets.** `NPM_TOKEN`, `TEST_PYPI_API_TOKEN`,
   `CARGO_TOKEN` and `CODECOV_TOKEN` all appear in workflow history but are
   referenced nowhere in the current tree — npm and PyPI moved to OIDC. If they
   still exist in the repository's secret store they are long-lived credentials
   with no remaining purpose, which is the easiest kind of thing to lose.

## Which PR runs get secrets, precisely

Worth stating exactly, because the usual shorthand — "PR checks don't have
secrets" — is true of the case people mean and false as a general rule, and
the difference is what made the LMAX episode above look harmless while it was
live.

**The rule is about where the head branch lives, not about the event.**

| Pull request from | Secrets | `GITHUB_TOKEN` |
|---|---|---|
| A **fork** | **None.** `secrets.X` resolves to the empty string | Read-only, forced by GitHub regardless of the repository default |
| A **branch in this repository** | **All of them**, exactly as on a push | The repository default (M1) |

So a same-repo pull request is *not* a reduced-privilege context. It is a
normal run that happens to be attached to a PR. That is what the
`fix-integration.yml` episode was: both `pull_request` runs came from the
base-repo branch `FIX-protocol`, so `LMAX_USERNAME`/`LMAX_PASSWORD` were
genuinely live in those runs — deliberately, to exercise the FIX session. No
fork could have reached them, and with one collaborator the population that
could open such a PR is one person. But "it was only on `pull_request`" would
have been the wrong reason to feel safe.

### For this repository, today

Three workflows trigger on `pull_request` — `rust-test.yml`,
`python-test.yml`, `security-audit.yml` — and **none of them references any
secret at all**, so the distinction above is currently academic here. What
keeps it that way:

- **No `pull_request_target`.** This is the trigger that runs the *base*
  repository's workflow against fork code **with** full secrets, and it is the
  usual root cause of public-repo CI compromise. Never present, in any commit.
- **No `workflow_run`.** The other escalation route: a privileged workflow
  that wakes on a fork PR's completion and consumes what it produced. Absent
  too. (`pypi-publish.yml` does use `download-artifact`, but only across jobs
  of its own run — `needs: [sdist, linux, macos, windows]` — which is not the
  cross-workflow pattern.)

### What a fork PR can still do without secrets

Not nothing, and worth knowing the shape of:

- **Run arbitrary code on the runner.** On `pull_request`, GitHub uses the
  workflow file from the PR head, so a fork PR can edit `rust-test.yml` and
  have its version execute. With no secrets and a read-only token, the runner
  is an ephemeral box with a public checkout — the practical loss is CI minutes
  and whatever the runner can reach outbound.
- **Read** the base branch's caches. It cannot **write** into them: GitHub
  scopes cache writes to the PR's own ref, and `main` cannot read a PR-scoped
  entry. So the cache-poisoning path in M3 is *not* reachable from a fork — it
  needs a push to a branch of this repository.
- Nothing else. It cannot push, tag, publish, or read a secret.

The first-time-contributor approval gate decides whether such a run *starts*;
it does not change what the run can access.

### The boundary is the merge button

Which is the same conclusion the section below reaches from the other
direction. Before the merge, a fork contributor has code execution and no
credentials. After it, their code runs on pushes to this repository, where
every secret is readable. There is no intermediate state, and nothing between
the two but a click.

## If a malicious workflow change were merged — the blast radius

The previous section asks whether anything *has* been abused. This asks the
harder question: **if a malicious change did get merged — approved in a hurry,
or slipped into an otherwise good PR — what could it reach?**

**Close to everything, and it would need no second mistake.** The merge is the
only gate this repository has. Everything after it is automatic.

### Why the merge is the whole gate

Four facts compound, and none of them is a finding above on its own:

1. **`main` is unprotected.** The GitHub API reports `protected: false` — no
   required review, no required status checks, no restriction on who pushes.
   The `if:` guards on the test legs are the only thing resembling a gate, and
   M5 shows those are keyed on commit-message text.
2. **A merge is enough to make code run — no dispatch needed.** Sixteen
   workflows fire on `push`, and eleven of them carry no `branches:` filter at
   all, so they run on a push to *any* branch, not just `main` (L5).
3. **Any workflow on a branch of this repository can read any secret.** GitHub
   has no per-workflow secret ACL. `secrets.CRATES_IO_API_TOKEN` resolves just
   as happily inside `redis-integration.yml` as inside `crates-publish.yml`.
   Two added lines in the least-watched integration workflow is the whole
   exploit.
4. **No workflow declares an `environment:`.** Not one, and not once in the
   entire history of the repository. An environment is the control that makes
   secret access require a human approval *at run time* and restricts it to
   named branches — it is the only mechanism that survives a bad merge, and it
   is absent.

Log masking is not a mitigation here. `::add-mask::` and GitHub's automatic
redaction only rewrite log output; an exfiltration step encodes or splits the
value and posts it outbound, and the run looks clean.

### What is reachable

| Credential | Reachable after a merge? | Consequence |
|---|---|---|
| `CRATES_IO_API_TOKEN` | **Yes**, from any workflow | Long-lived. Publish rights over all five published crates — a backdoored `wingfoil` reaches every downstream user |
| `WF_REBASE_PAT` | **Yes** | A *classic* PAT, which cannot be scoped to one repository — it likely carries the owner's access across every repo they can reach. The worst single item in the store |
| `PULUMI_ACCESS_TOKEN` | **Yes** | Full Pulumi Cloud state, including the stack config the LMAX credentials are written into |
| `LMAX_USERNAME` / `LMAX_PASSWORD` | **Yes** | Live FIX trading credentials |
| `KDB_LICENSE_B64` | **Yes** | Licence exposure |
| npm / PyPI publishing (OIDC) | **Yes** | Trusted publishing is scoped to repo + *workflow filename*, with **no environment** (noted as deliberate in `pypi-publish.yml`). Filename scoping stops an unrelated workflow, not someone who can edit `npm-publish.yml` — adding a `push:` trigger to it mints a real token automatically |
| AWS role (`AWS_ROLE_TO_ASSUME`) | **Depends — go and check** | The ARN is not a secret; what decides this is the IAM role's OIDC trust policy. If its `sub` condition is the common `repo:wingfoil-io/wingfoil:*`, any workflow on any branch can assume it, reaching EC2/ECS/ECR/SSM. If it is pinned to a ref or environment, much narrower. **This is not visible from the repository and is the highest-value thing to go verify.** |
| `GITHUB_TOKEN` | Depends on the repo default (M1) | If still read-write, push to `main`, cut releases, edit issues |

`id-token: write` is not a barrier either — a merge-capable attacker simply
declares it on their own job.

### The path that touches no secret at all

Worth stating separately, because it defeats "I would notice a suspicious
`curl`":

Poison the `wasm-pub` Rust build cache (M3 — writable from any run, no
`save-if` on any of its three writers), then wait. The next time the owner
dispatches `publish.npm`, that job *restores* the cache, builds from it, and
publishes to npm **with provenance**. The attestation then truthfully vouches
for a malicious artifact, because provenance attests to where a build ran, not
to what went into it. The same shape applies to the `type=gha` Docker cache
feeding the ECR images and the deployed demo.

### "They were a previous contributor" — this part is not the risk

Worth separating clearly, because it is the intuitive fear and it is the wrong
one:

- **A past contributor holds no standing access.** There is no "trusted
  contributor" tier in GitHub that grants secrets, and a merged PR confers
  nothing persistent. This repository has one collaborator; everyone else is a
  fork-PR author, first PR or fiftieth.
- **What does change after their first merged PR** is that GitHub's default
  "require approval for first-time contributors" stops applying to them, so
  their fork-PR runs start without a maintainer click. Those runs still get
  **no secrets and a read-only token**. They can spend CI minutes, execute
  arbitrary code on an ephemeral runner, and poison a cache scoped to their own
  PR ref — which `main` cannot read.
- **Approving a workflow *run* is not the dangerous button.** Merging is.
  "Approve and run" on a fork PR grants execution, not secrets.

So: *previous contributor* ≈ no meaningful elevation. *Merged their PR* ≈ the
full table above.

### A correction to M2 above

`secrets: inherit` was ranked Medium partly on blast radius, and under **this**
threat model that ranking is too generous to the alternative: an attacker who
can merge can name any secret in any workflow whether or not `inherit` is
present, so removing it would not have stopped any of the above. It remains
worth removing — it costs an audit signal, and it is how a *reused* workflow
leaks — but it is not a control, and it should not be mistaken for one. The
control is environments.

### What would actually stop it

Ranked for this threat model specifically, which is a different order from the
list at the end of this document:

1. **GitHub Environments with required reviewers**, on every job that touches a
   secret — the three publish workflows and the deploy/AMI/image ones. This is
   the single control that survives a malicious merge: secret access becomes a
   human decision at run time, restricted to `main`. Register the npm and PyPI
   trusted publishers **with** that environment too, rather than the current
   deliberate no-environment setup.
2. **Branch protection (or a ruleset) on `main`** — currently none at all.
   Require a pull request, a review, and passing checks, with bypass disabled.
   With a single maintainer this reads like theatre; it is what turns "I
   approved it" from the default path into a deliberate act.
3. **Pin the AWS OIDC trust policy** to a specific ref and/or environment
   rather than `repo:wingfoil-io/wingfoil:*`.
4. **Retire the classic PAT** for a GitHub App installation token or a
   fine-grained token scoped to this repository and `contents: write`.
5. **Move crates.io to trusted publishing**, retiring the last long-lived
   registry token.
6. Then the cache hygiene (M3), and the rest of the list.

Note also that recovery would be difficult: with no audit-log access
configured and step logs expiring at ~90 days, a successful exfiltration would
leave little to find after the fact. Rotation is the only reliable response,
which is an argument for making the credentials cheap to rotate now.

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
