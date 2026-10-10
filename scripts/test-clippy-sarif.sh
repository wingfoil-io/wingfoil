#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TEST_ROOT="$(mktemp -d)"
trap 'rm -rf "$TEST_ROOT"' EXIT
mkdir "$TEST_ROOT/bin"
export CALLS="$TEST_ROOT/cargo-calls"
export REAL_TEE
REAL_TEE=$(command -v tee)

cat >"$TEST_ROOT/bin/cargo" <<'MOCK'
#!/usr/bin/env bash
printf '%s\n' "$*" >>"$CALLS"
printf '%s\n' '{"diagnostic":"test"}'
exit "${CARGO_STATUS:-0}"
MOCK

cat >"$TEST_ROOT/bin/clippy-sarif" <<'MOCK'
#!/usr/bin/env bash
cat
exit "${CONVERTER_STATUS:-0}"
MOCK

cat >"$TEST_ROOT/bin/tee" <<'MOCK'
#!/usr/bin/env bash
"$REAL_TEE" "$@"
exit "${TEE_STATUS:-0}"
MOCK

cat >"$TEST_ROOT/bin/sarif-fmt" <<'MOCK'
#!/usr/bin/env bash
cat
exit "${FORMATTER_STATUS:-0}"
MOCK

chmod +x "$TEST_ROOT/bin/"*
export PATH="$TEST_ROOT/bin:$PATH"
cd "$TEST_ROOT"

for failing in none cargo converter tee formatter; do
    export CARGO_STATUS=0 CONVERTER_STATUS=0 TEE_STATUS=0 FORMATTER_STATUS=0
    expected=0
    case "$failing" in
        cargo) CARGO_STATUS=101; expected=101 ;;
        converter) CONVERTER_STATUS=2; expected=2 ;;
        tee) TEE_STATUS=3; expected=3 ;;
        formatter) FORMATTER_STATUS=4; expected=4 ;;
    esac
    : >"$CALLS"
    status=0
    bash "$SCRIPT_DIR/clippy-sarif.sh" >rendered || status=$?
    [[ "$status" == "$expected" ]] || { echo "FAIL: $failing returned $status, expected $expected" >&2; exit 1; }
    # Exact arguments and a single line pin one strict, locked all-feature pass.
    [[ "$(cat "$CALLS")" == 'clippy --locked --workspace --all-targets --all-features --message-format=json -- -D warnings' ]]
    [[ "$(cat rust-clippy-results.sarif)" == '{"diagnostic":"test"}' ]]
    cmp rust-clippy-results.sarif rendered
    echo "PASS: $failing (exit $status; report preserved)"
done
