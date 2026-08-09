# Security Policy

## Reporting a vulnerability

**Please do not open a public issue for a security vulnerability.**

Report it privately through either channel:

- [GitHub private vulnerability reporting](https://github.com/wingfoil-io/wingfoil/security/advisories/new)
  — preferred, and keeps the report attached to the repository.
- Email `hello@wingfoil.io` with `SECURITY` in the subject line.

Please include enough to reproduce: the version (or commit), the feature flags
enabled, the adapter involved if any, and a minimal graph or test that shows
the problem.

We aim to acknowledge a report within three working days, and to keep you
updated as we work through it. When a fix ships we will credit you in the
advisory unless you would rather we didn't.

## Supported versions

Wingfoil has not yet reached a long-term-support release. Fixes land on the
latest published version; there is no backporting to earlier majors. See the
[releases](https://github.com/wingfoil-io/wingfoil/releases) for what is
current.

## Scope

In scope:

- The engine and runtime (`crates/wingfoil`), including anything reachable
  from a graph built with untrusted input.
- The I/O adapters — in particular the ones parsing bytes off a network
  (`fix`, `web`, `aeron`, `zmq`, `kafka`, `redis`, `etcd`) or reading files
  from disk (`csv`, `lines`, `kdb`).
- The Python bindings (`crates/wingfoil-python`) and the WASM/TypeScript
  client, where a memory-safety or sandbox-escape issue would cross a
  language boundary.

Out of scope:

- Vulnerabilities in third-party services the adapters talk to — report those
  upstream.
- Denial of service achieved only by configuring a graph to consume unbounded
  resources. Wingfoil runs the graph you wire; it is not a sandbox for
  untrusted graph definitions.
- Findings from automated scanners with no demonstrated impact.

## Dependency vulnerabilities

Dependency advisories are caught two ways, and the difference matters:

- [`security-audit.yml`](.github/workflows/security-audit.yml) **fails CI** on a
  dependency with a known advisory — `cargo audit` against RustSec for both
  Cargo workspaces, `pnpm audit` for `js/`, and `dependency-review` to block a
  pull request that *introduces* a vulnerable dep. It also runs weekly, so an
  advisory disclosed against an already-pinned dependency surfaces without a
  code change.
- **Dependabot security updates** open the upgrade PRs, automatically, against
  the GitHub Advisory Database. These are a repository setting rather than a
  `dependabot.yml` entry, so they cover every ecosystem Dependabot can parse.

  Dependabot **version updates** — routine bumps of dependencies with no
  advisory against them — are deliberately **not** enabled. They are a
  different trade to the one above: staying at the tip of every dependency
  shortens the distance to a future security fix, but it also puts this
  repository in the first wave to install any newly published release, which
  is exactly the population a compromised-maintainer attack targets. Note that
  neither `cargo audit` nor `pnpm audit` defends against that — they match
  against advisory databases, and a freshly malicious release has no advisory
  yet by construction.

  Upgrade deliberately instead: `cargo update` / `pnpm update` when there is a
  reason to, and read what moved. If version updates are ever reinstated, they
  should carry a cooldown (Dependabot now defaults to three days, and
  `semver-major-days` can be set much higher) and `js/` should set pnpm's
  `minimumReleaseAge`.

You are welcome to open a normal public issue for a dependency advisory — they
are already public by definition.
