#!/usr/bin/env bash
#
# Verify that every wingfoil example is documented.
#
# For each `[[example]]` target declared in crates/wingfoil/Cargo.toml:
#   1. its directory must contain a README.md, and
#   2. that directory must be linked from its group's README.md
#      (examples/{core,adapters,showcase}/README.md).
#
# Also checks that each group README is itself linked from examples/README.md.
#
# Run from anywhere:  scripts/check-example-docs.sh
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate="$repo_root/crates/wingfoil"
examples="$crate/examples"
manifest="$crate/Cargo.toml"

fail=0
note() { printf '  %s\n' "$1"; fail=1; }

if [[ ! -f "$manifest" ]]; then
    echo "error: cannot find $manifest" >&2
    exit 2
fi

# --- Collect declared example targets: "<name> <path>" -----------------------
# Parsed straight from the manifest so the check cannot drift from what cargo
# actually builds.
targets=$(
    awk '
        # Flush first, so a block directly followed by another is not lost.
        /^\[/              { if (in_block && name != "") print name, path; in_block = 0 }
        /^\[\[example\]\]/ { in_block = 1; name = ""; path = ""; next }
        in_block && /^name[[:space:]]*=/ { split($0, a, "\""); name = a[2] }
        in_block && /^path[[:space:]]*=/ { split($0, a, "\""); path = a[2] }
        END                { if (in_block && name != "") print name, path }
    ' "$manifest"
)

if [[ -z "$targets" ]]; then
    echo "error: no [[example]] targets found in $manifest" >&2
    exit 2
fi

echo "Checking example documentation..."

# --- 1 & 2: every target has a README, and is linked from its group ----------
while read -r name path; do
    [[ -z "$name" ]] && continue

    if [[ -z "$path" ]]; then
        note "$name: no explicit \`path\` (autoexamples is off; add one)"
        continue
    fi

    abs="$crate/$path"
    if [[ ! -f "$abs" ]]; then
        note "$name: declared path does not exist: $path"
        continue
    fi

    dir="$(dirname "$abs")"
    if [[ ! -f "$dir/README.md" ]]; then
        note "$name: no README.md in $(realpath --relative-to="$repo_root" "$dir")"
    fi

    # The group is the first path component under examples/.
    rel="${path#examples/}"
    group="${rel%%/*}"
    group_readme="$examples/$group/README.md"

    if [[ ! -f "$group_readme" ]]; then
        note "$name: no group README at examples/$group/README.md"
        continue
    fi

    # The example's directory name, relative to its group, must appear as a
    # link target somewhere in the group README. Nested examples (kdb/read)
    # may be linked via their parent, so accept either.
    sub="$(realpath --relative-to="$examples/$group" "$dir")"
    top="${sub%%/*}"
    if ! grep -q "($sub/)\|($top/)" "$group_readme"; then
        note "$name: $group/$sub is not linked from examples/$group/README.md"
    fi
done <<< "$targets"

# --- 3: each group README is linked from the examples front door -------------
front="$examples/README.md"
if [[ ! -f "$front" ]]; then
    note "missing examples/README.md"
else
    for group in core adapters showcase; do
        [[ -d "$examples/$group" ]] || continue
        grep -q "($group/)" "$front" || note "examples/README.md does not link $group/"
    done
fi

if [[ $fail -ne 0 ]]; then
    echo
    echo "Example documentation check FAILED."
    echo "Every example needs a README.md beside it and a link from its group's index."
    echo "See crates/wingfoil/examples/README.md § 'Adding an example'."
    exit 1
fi

# --- 4: the count stated in the repository README must be true ---------------
# README.md says how many examples there are out loud, which is exactly the
# kind of claim that goes stale the moment someone adds one — and a new example
# is a new *file*, so nothing else in this check notices. The README sentence is
# the only place the number is written down; this compares it against what the
# manifest actually declares, so there is no second constant to keep in sync.
#
# The README deliberately does NOT mention this check — a reader counting the
# examples does not care how the number is kept honest, and the note was in the
# way of the sentence that matters. That is why the sed pattern below is the
# contract: it is the only thing tying the two together, so keep it and the
# README wording in step, and prefer editing both here over quietly relaxing it.
# `tr -d` because BSD `wc` pads its output with spaces and these are compared
# as strings, not just printed.
count=$(wc -l <<< "$targets" | tr -d '[:space:]')
dirs=$(while read -r _ path; do [[ -n "$path" ]] && dirname "$path"; done <<< "$targets" | sort -u | wc -l | tr -d '[:space:]')

readme="$repo_root/README.md"
claimed=$(sed -n 's/^There are \([0-9][0-9]*\) runnable example targets (\([0-9][0-9]*\) directories).*/\1 \2/p' "$readme")

if [[ -z "$claimed" ]]; then
    echo
    echo "Example documentation check FAILED."
    echo "README.md no longer states the example count in the form this check reads:"
    echo "  There are N runnable example targets (M directories) ..."
    echo "Restore that sentence, or update the pattern here — do not just delete it."
    exit 1
fi

if [[ "$claimed" != "$count $dirs" ]]; then
    echo
    echo "Example documentation check FAILED."
    echo "README.md claims $(cut -d' ' -f1 <<< "$claimed") targets in $(cut -d' ' -f2 <<< "$claimed") directories;"
    echo "crates/wingfoil/Cargo.toml declares $count targets in $dirs directories."
    echo "Update the sentence near the top of README.md."
    exit 1
fi

echo "OK — $count example targets in $dirs directories, all documented and indexed."
