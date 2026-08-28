<!--
Thanks for contributing. Keep this short — the diff is the detail, this is
the orientation. Delete any section that doesn't apply.
-->

## What this changes

<!-- One or two sentences. What is different after this lands? -->

## Why

<!-- The problem being solved. Link the issue if there is one: "Closes #123". -->

## How it was verified

<!--
Which of these you ran, and anything that needed a service or a feature flag.
The pre-commit hook covers fmt and clippy; tests are the part worth naming.
-->

- [ ] `cargo fmt --all`
- [ ] `cargo lint` and `cargo lint-all`
- [ ] `cargo test -p wingfoil --all-features`
- [ ] New behaviour is covered by a test asserting **values and tick times**

## Notes for the reviewer

<!--
Anything that would be hard to see from the diff: a deliberate performance
trade-off, a follow-up you chose not to fold in.
-->

## Before you submit

<!--
One box, and it is the one most likely to cost you the merge. It applies to
PRs opened from a fork, which is most of them.
-->

- [ ] **Allow edits by maintainers** is ticked

<!--
Why this earns a section of its own: ops land at the same insertion points in
`ops.rs`, `fluent.rs` and `op_completeness.rs`, so an approved PR can pick up a
conflict within a day of a sibling merging. With this ticked, a maintainer
rebases your branch in one click and it lands as yours.

Without it GitHub refuses with "user doesn't have permission to update head
repository", and the ways out are asking you to rebase or carrying your commit
onto a fresh PR by hand. #896 was reviewed, approved, and then conflicted
across six files; it had to be re-landed as #902.

You can tick it after opening, too — it is in the PR's right-hand sidebar.
-->
