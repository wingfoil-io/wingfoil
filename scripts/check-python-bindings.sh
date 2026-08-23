#!/usr/bin/env bash
#
# Verify that every wingfoil-python adapter binding is fully registered.
#
# A new adapter binding has to be declared in six places, and nothing used to
# notice a missed one: the build simply compiled the binding out, so the
# adapter was silently absent from the wheel. This is the binding-side twin of
# check-example-docs.sh.
#
# For each crates/wingfoil-python/src/adapters/<name>.rs:
#   1. `pub mod <name>;` in src/adapters/mod.rs, gated on `feature = "<name>"`
#   2. a `<name> = [...]` feature in Cargo.toml
#   3. that feature enables `_common` (the shared async broker — every adapter
#      module names async_source::RunParams via adapters::common)
#   4. that feature is in the `all-adapters` roll-up
#   5. its functions are registered in python.rs (`crate::adapters::<name>::`)
#   6. the feature is in pyproject.toml's maturin list, i.e. in the wheel
#
# Check 6 has two standing exemptions, both deliberate and documented in
# Cargo.toml: `aeron` (builds the Aeron C library, needs clang/CMake >= 3.30)
# and `iceoryx2` (Linux/POSIX-only). Both are in `all-adapters` and so in the
# Linux wheel, but out of the portable default set. Adding to this list is a
# decision — record the reason here when you do.
#
# Run from anywhere:  scripts/check-python-bindings.sh
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate="$repo_root/crates/wingfoil-python"
adapters="$crate/src/adapters"
manifest="$crate/Cargo.toml"
pyproject="$crate/pyproject.toml"
register="$crate/src/python.rs"
modrs="$adapters/mod.rs"

# Features intentionally kept out of the portable wheel (check 6 only).
wheel_exempt="aeron iceoryx2"

fail=0
note() { printf '  %s\n' "$1"; fail=1; }

for f in "$manifest" "$pyproject" "$register" "$modrs"; do
    if [[ ! -f "$f" ]]; then
        echo "error: cannot find $f" >&2
        exit 2
    fi
done

# The `all-adapters` roll-up and the maturin list, flattened to space-delimited
# words so membership is a substring test on " <name> ".
all_adapters=" $(awk '
    /^all-adapters[[:space:]]*=/ { in_b = 1 }
    in_b                        { print }
    in_b && /\]/                { exit }
' "$manifest" | grep -o '"[a-z0-9_-]*"' | tr -d '"' | tr '\n' ' ')"

wheel_features=" $(awk '
    /^features[[:space:]]*=/ { in_b = 1 }
    in_b                     { print }
    in_b && /\]/             { exit }
' "$pyproject" | grep -o '"[a-z0-9_-]*"' | tr -d '"' | tr '\n' ' ')"

echo "Checking wingfoil-python adapter binding registration..."

count=0
for path in "$adapters"/*.rs; do
    name="$(basename "$path" .rs)"
    # `mod` is the module list itself; `common` is the shared internal helper,
    # not an adapter.
    [[ "$name" == "mod" || "$name" == "common" ]] && continue
    count=$((count + 1))

    # 1 + 2 of the module declaration: present, and feature-gated.
    if ! grep -q "^pub mod $name;" "$modrs"; then
        note "$name: no \`pub mod $name;\` in src/adapters/mod.rs"
    elif ! grep -B1 "^pub mod $name;" "$modrs" | grep -q "cfg(feature = \"$name\")"; then
        note "$name: \`pub mod $name;\` is not gated on \`feature = \"$name\"\`"
    fi

    # 2. The feature exists at all.
    feature_line="$(grep -m1 "^$name = \[" "$manifest" || true)"
    if [[ -z "$feature_line" ]]; then
        note "$name: no \`$name = [...]\` feature in Cargo.toml"
    else
        # 3. It enables the shared async broker.
        if [[ "$feature_line" != *'"_common"'* ]]; then
            note "$name: feature does not enable \`_common\` (needed for async_source::RunParams)"
        fi
    fi

    # 4. In the roll-up CI and the Linux wheel build from.
    if [[ "$all_adapters" != *" $name "* ]]; then
        note "$name: not in the \`all-adapters\` roll-up in Cargo.toml"
    fi

    # 5. Its functions reach the module.
    if ! grep -q "crate::adapters::$name::" "$register"; then
        note "$name: no \`crate::adapters::$name::\` registration in src/python.rs"
    fi

    # 6. In the wheel, unless deliberately exempt.
    if [[ "$wheel_features" != *" $name "* && " $wheel_exempt " != *" $name "* ]]; then
        note "$name: not in pyproject.toml's maturin \`features\` list (and not a documented exemption)"
    fi
done

# The reverse direction: a roll-up entry with no module behind it.
for name in $all_adapters; do
    [[ -z "$name" ]] && continue
    if [[ ! -f "$adapters/$name.rs" ]]; then
        note "$name: in \`all-adapters\` but there is no src/adapters/$name.rs"
    fi
done

if [[ $count -eq 0 ]]; then
    echo "error: no adapter modules found in $adapters" >&2
    exit 2
fi

if [[ $fail -ne 0 ]]; then
    echo
    echo "Python binding registration check FAILED."
    echo "A binding must be declared in all six places or it is silently"
    echo "compiled out of the wheel. See .claude/commands/bind-adapter.md."
    exit 1
fi

echo "OK — $count adapter bindings, all fully registered."
