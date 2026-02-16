# Release Process

Run these steps in this order.  Wait for each step to complete successfully before starting the next step.

* Bump version
* crates.io pub
* pypi pub


# Commit hooks

These checks must succeed:

* cargo clippy --workspace --all-targets --all-features
* cargo fmt --all -- --check
