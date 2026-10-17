#!/usr/bin/env bash
# Exercise the actual inline workflow script; all mutating Git commands are mocked.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
export LOG="$tmp/git.log"

# Extract this named step's literal run block, stopping at the next YAML field.
awk '
  { sub(/\r$/, "") }
  /^      - name: Rebase selected branches$/ { selected=1; next }
  selected && /^        run: \|$/ { script=1; next }
  script && NF && !/^          / { exit }
  script { sub(/^          /, ""); print }
' "$root/.github/workflows/bulk-rebase.yml" > "$tmp/run.sh"
test -s "$tmp/run.sh"

git() {
    case "$1" in
        check-ref-format) command git "$@" ;;
        show-ref)
            case "$4" in
                refs/remotes/origin/topic/one|refs/remotes/origin/topic/two|refs/remotes/origin/topic\;literal) return 0 ;;
                *) return 1 ;;
            esac ;;
        checkout|rebase|push)
            printf '%s\n' "$*" >> "$LOG"
            if [[ "$1" == rebase && "$2" != --abort && "${REBASE_FAIL:-}" == yes ]]; then
                unset REBASE_FAIL
                return 1
            fi
            if [[ "$1" == push && "${PUSH_FAIL:-}" == yes ]]; then
                return 1
            fi ;;
        *) echo "Unexpected git command: $*" >&2; return 1 ;;
    esac
}
export -f git

run() {
    : > "$LOG"
    BRANCHES="$1" bash -e -o pipefail "$tmp/run.sh" > "$tmp/output" 2>&1
}

run $'topic/one\n\ttopic/two\r\n'
cat > "$tmp/expected" <<'EOF'
checkout --detach refs/remotes/origin/topic/one
rebase refs/remotes/origin/main
push origin HEAD:refs/heads/topic/one --force-with-lease
checkout --detach refs/remotes/origin/topic/two
rebase refs/remotes/origin/main
push origin HEAD:refs/heads/topic/two --force-with-lease
EOF
diff -u "$tmp/expected" "$LOG"

run 'topic/one'
head -n 3 "$tmp/expected" > "$tmp/single"
diff -u "$tmp/single" "$LOG"

for invalid in '' ' ' main HEAD --all 'topic/*' 'topic/../one' 'topic/one:other' \
    '@{-1}' missing 'topic/one missing' 'topic/one main'; do
    if run "$invalid"; then
        echo "Unexpected success for: $invalid" >&2
        exit 1
    fi
    test ! -s "$LOG" # Even a later invalid entry must prevent the first checkout.
done

run 'topic;literal'
grep -Fx 'push origin HEAD:refs/heads/topic;literal --force-with-lease' "$LOG"

REBASE_FAIL=yes run 'topic/one topic/two'
test "$(grep -c '^rebase --abort$' "$LOG")" -eq 1
test "$(grep -c '^push ' "$LOG")" -eq 1
grep -Fx 'push origin HEAD:refs/heads/topic/two --force-with-lease' "$LOG"

if PUSH_FAIL=yes run 'topic/one topic/two'; then
    echo 'A rejected push must fail the workflow.' >&2
    exit 1
fi
test "$(grep -c '^checkout ' "$LOG")" -eq 1
echo 'PASS: selection, preflight rejection, literal input, conflict recovery, push failure'
