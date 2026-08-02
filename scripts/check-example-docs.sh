#!/usr/bin/env bash
#
# Verify that every wingfoil-next example is documented.
#
# For each `[[example]]` target declared in crates/wingfoil-next/Cargo.toml:
#   1. its directory must contain a README.md, and
#   2. that directory must be linked from its group's README.md
#      (examples/{core,adapters,showcase}/README.md).
#
# Also checks that each group README is itself linked from examples/README.md.
#
# Run from anywhere:  scripts/check-example-docs.sh
set -uo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
crate="$repo_root/crates/wingfoil-next"
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
    echo "See next/crates/wingfoil-next/examples/README.md § 'Adding an example'."
    exit 1
fi

count=$(wc -l <<< "$targets")
echo "OK — $count example targets, all documented and indexed."
