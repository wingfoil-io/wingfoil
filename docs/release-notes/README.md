# Release notes

One page per release: what changed, why, and what you have to do about it.

These are the curated, human-written half of a release. The commit-level
changelog is generated per tag on the
[GitHub releases page](https://github.com/wingfoil-io/wingfoil/releases), and
the release body links here for the part a generated changelog cannot write —
rationale, upgrade steps, and what was deliberately left out.

| Version | |
|---|---|
| [**9.0.0**](9.0.0.md) | The engine cutover — the `Op` engine replaces `MutableNode`. Breaking; [Rust](../migration.md) and [Python](../../crates/wingfoil-python/docs/migration.rst) migration guides |

Releases before 9.0.0 have no page here — the practice starts with the cutover,
which is the first release that needed one.

## Adding a page

Name the file after the version (`9.1.0.md`), add a row to the table above
(newest first), and lead with what the release *is* before what it contains. A
release that breaks something owes three things: the reasoning behind the break,
the shortest upgrade path that actually works, and an honest list of what is
gone or deferred. A release that breaks nothing can be short.
