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
