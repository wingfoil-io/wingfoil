#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SETUP_SCRIPT="$SCRIPT_DIR/setup-dev.sh"
TEST_ROOT="$(mktemp -d)"
REAL_BASH=$(command -v bash)
trap 'rm -rf "$TEST_ROOT"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local haystack=$1
    local needle=$2
    [[ "$haystack" == *"$needle"* ]] || fail "expected '$needle' in '$haystack'"
}

assert_not_contains() {
    local haystack=$1
    local needle=$2
    [[ "$haystack" != *"$needle"* ]] || fail "did not expect '$needle' in '$haystack'"
}

write_mock() {
    local name=$1
    cat >"$MOCK_BIN/$name"
    chmod +x "$MOCK_BIN/$name"
}

new_case() {
    local name=$1
    CASE_DIR="$TEST_ROOT/$name"
    MOCK_BIN="$CASE_DIR/bin"
    MOCK_STATE="$CASE_DIR/state"
    MOCK_LOG="$CASE_DIR/calls.log"
    mkdir -p "$MOCK_BIN" "$MOCK_STATE"
    : >"$MOCK_LOG"

    write_mock uname <<'MOCK'
#!/bin/bash
case "${1:-}" in
    -m) printf '%s\n' "${MOCK_ARCH:-x86_64}" ;;
    *) printf '%s\n' "${MOCK_OS:-Linux}" ;;
esac
MOCK

    write_mock protoc <<'MOCK'
#!/bin/bash
echo "libprotoc 29.0"
MOCK

    write_mock clang <<'MOCK'
#!/bin/bash
echo "clang version 18.1.0"
MOCK

    write_mock cmake <<'MOCK'
#!/bin/bash
if [[ -f "$MOCK_STATE/cmake-new" ]]; then
    echo "cmake version 3.31.0"
else
    echo "cmake version ${MOCK_CMAKE_VERSION:-3.31.0}"
fi
MOCK

    write_mock pkg-config <<'MOCK'
#!/bin/bash
if [[ "${1:-}" == "--modversion" ]]; then
    [[ -f "$MOCK_STATE/native-installed" ]] || exit 1
    echo "4.3.5"
    exit 0
fi
[[ -f "$MOCK_STATE/native-installed" ]]
MOCK

    write_mock dpkg-query <<'MOCK'
#!/bin/bash
[[ -f "$MOCK_STATE/native-installed" ]] || exit 1
echo "install ok installed"
MOCK

    write_mock sudo <<'MOCK'
#!/bin/bash
printf 'sudo %s\n' "$*" >>"$MOCK_LOG"
"$@"
MOCK

    write_mock env <<'MOCK'
#!/bin/bash
while (($#)) && [[ $1 == *=* ]]; do
    export "$1"
    shift
done
exec "$@"
MOCK

    write_mock sh <<'MOCK'
#!/bin/bash
printf 'sh %s\n' "$*" >>"$MOCK_LOG"
exec /bin/bash "$@"
MOCK

    write_mock mktemp <<'MOCK'
#!/bin/bash
echo "$MOCK_STATE/cmake-installer.sh"
MOCK

    write_mock rm <<'MOCK'
#!/bin/bash
for target in "$@"; do
    [[ "$target" == -* ]] || : >"$target.removed"
done
MOCK

    write_mock curl <<'MOCK'
#!/bin/bash
printf 'curl %s\n' "$*" >>"$MOCK_LOG"
output=
while (($#)); do
    if [[ "$1" == "-o" ]]; then
        shift
        output=$1
    fi
    shift
done
[[ -n "$output" ]]
printf '#!/bin/bash\n: >"$MOCK_STATE/cmake-new"\n' >"$output"
MOCK

    write_mock sha256sum <<'MOCK'
#!/bin/bash
printf 'sha256sum %s\n' "$*" >>"$MOCK_LOG"
if [[ ${MOCK_BAD_CHECKSUM:-false} == true ]]; then
    printf '%064d  %s\n' 0 "$1"
else
    echo "7cfdf4a411c71d13c027199952fd25e8245d85c932ff452e2b9a9e0f6dfe368a  $1"
fi
MOCK
}

add_package_manager() {
    local name=$1
    write_mock "$name" <<'MOCK'
#!/bin/bash
command_name=${0##*/}
printf '%s %s\n' "$command_name" "$*" >>"$MOCK_LOG"
case "$command_name $*" in
    "brew --prefix llvm")
        echo /opt/homebrew/opt/llvm
        exit
        ;;
    "dnf list --installed "*|"pacman -Q "*|"brew list --versions "*)
        [[ -f "$MOCK_STATE/native-installed" ]]
        exit
        ;;
esac
case " $* " in
    *" install "*|*" -S "*)
        : >"$MOCK_STATE/native-installed"
        ;;
esac
MOCK
}

run_setup() {
    set +e
    RUN_OUTPUT=$(env \
        PATH="$MOCK_BIN" \
        MOCK_ARCH="${MOCK_ARCH:-x86_64}" \
        MOCK_BAD_CHECKSUM="${MOCK_BAD_CHECKSUM:-false}" \
        MOCK_CMAKE_VERSION="${MOCK_CMAKE_VERSION:-3.31.0}" \
        MOCK_LOG="$MOCK_LOG" \
        MOCK_OS="${MOCK_OS:-Linux}" \
        MOCK_STATE="$MOCK_STATE" \
        "$REAL_BASH" "$SETUP_SCRIPT" "$@" 2>&1)
    RUN_STATUS=$?
    set -e
}

new_case bare
run_setup
[[ $RUN_STATUS -eq 0 ]] || fail "bare setup failed with $RUN_STATUS: $RUN_OUTPUT"
[[ "$RUN_OUTPUT" == "protoc already installed: libprotoc 29.0" ]] || \
    fail "bare setup output changed: $RUN_OUTPUT"
[[ ! -s "$MOCK_LOG" ]] || fail "bare setup attempted native installation"

new_case apt-old-cmake
add_package_manager apt-get
MOCK_CMAKE_VERSION=3.28.3 run_setup --all-features
[[ $RUN_STATUS -eq 0 ]] || fail "apt all-features setup failed: $RUN_OUTPUT"
CALLS=$(<"$MOCK_LOG")
assert_contains "$CALLS" "apt-get update"
assert_contains "$CALLS" "apt-get install -y build-essential ca-certificates curl clang libclang-dev uuid-dev libbsd-dev cmake libzmq3-dev libssl-dev pkg-config"
assert_contains "$CALLS" "curl -fsSL"
assert_contains "$CALLS" "sha256sum $MOCK_STATE/cmake-installer.sh"
assert_contains "$CALLS" "sh $MOCK_STATE/cmake-installer.sh --prefix=/usr/local --skip-license --exclude-subdir"
if ((EUID != 0)); then
    assert_contains "$CALLS" "sudo sh $MOCK_STATE/cmake-installer.sh --prefix=/usr/local --skip-license --exclude-subdir"
fi
assert_contains "$RUN_OUTPUT" "CMake 3.28.3 is too old; installing CMake 3.31.0"
assert_contains "$RUN_OUTPUT" "Verified CMake 3.31.0 installer SHA-256"
assert_contains "$RUN_OUTPUT" "Native all-features toolchain ready"

: >"$MOCK_LOG"
run_setup --all-features
[[ $RUN_STATUS -eq 0 ]] || fail "idempotent apt rerun failed: $RUN_OUTPUT"
[[ ! -s "$MOCK_LOG" ]] || fail "idempotent apt rerun attempted installation: $(<"$MOCK_LOG")"
assert_contains "$RUN_OUTPUT" "Native packages already installed"
assert_contains "$RUN_OUTPUT" "CMake already installed: 3.31.0"

for package_manager in dnf pacman; do
    new_case "$package_manager"
    add_package_manager "$package_manager"
    run_setup --all-features
    [[ $RUN_STATUS -eq 0 ]] || fail "$package_manager all-features setup failed: $RUN_OUTPUT"
    CALLS=$(<"$MOCK_LOG")
    case "$package_manager" in
        dnf)
            assert_contains "$CALLS" "dnf install -y gcc-c++ make ca-certificates curl clang clang-devel libuuid-devel libbsd-devel cmake zeromq-devel openssl-devel pkgconf-pkg-config"
            ;;
        pacman)
            assert_contains "$CALLS" "pacman -S --noconfirm base-devel ca-certificates curl clang util-linux-libs libbsd cmake zeromq openssl pkgconf"
            ;;
    esac
done

new_case dnf-epel
write_mock dnf <<'MOCK'
#!/bin/bash
printf 'dnf %s\n' "$*" >>"$MOCK_LOG"
case "$*" in
    "list --installed "*) [[ -f "$MOCK_STATE/native-installed" ]] ;;
    "list --available libbsd-devel") [[ -f "$MOCK_STATE/epel-enabled" ]] ;;
    "install -y epel-release") : >"$MOCK_STATE/epel-enabled" ;;
    "install -y "*) : >"$MOCK_STATE/native-installed" ;;
esac
MOCK
run_setup --all-features
[[ $RUN_STATUS -eq 0 ]] || fail "dnf EPEL setup failed: $RUN_OUTPUT"
CALLS=$(<"$MOCK_LOG")
assert_contains "$CALLS" "dnf install -y epel-release"
assert_contains "$CALLS" "dnf install -y gcc-c++ make ca-certificates curl clang clang-devel libuuid-devel libbsd-devel cmake zeromq-devel openssl-devel pkgconf-pkg-config"

new_case bad-cmake-checksum
add_package_manager apt-get
MOCK_CMAKE_VERSION=3.28.3 MOCK_BAD_CHECKSUM=true run_setup --all-features
[[ $RUN_STATUS -ne 0 ]] || fail "bad CMake checksum unexpectedly succeeded"
CALLS=$(<"$MOCK_LOG")
assert_contains "$RUN_OUTPUT" "CMake installer checksum mismatch"
assert_not_contains "$CALLS" "sh $MOCK_STATE/cmake-installer.sh"

new_case brew
add_package_manager brew
MOCK_OS=Darwin run_setup --all-features
[[ $RUN_STATUS -eq 0 ]] || fail "brew all-features setup failed: $RUN_OUTPUT"
CALLS=$(<"$MOCK_LOG")
assert_contains "$CALLS" "brew install llvm cmake zeromq openssl@3 pkgconf"
assert_contains "$CALLS" "brew --prefix llvm"
assert_not_contains "$CALLS" "sudo brew"
assert_contains "$RUN_OUTPUT" 'set LIBCLANG_PATH="/opt/homebrew/opt/llvm/lib"'

new_case help
run_setup --help
[[ $RUN_STATUS -eq 0 ]] || fail "--help returned $RUN_STATUS instead of 0"
[[ "$RUN_OUTPUT" == "Usage: scripts/setup-dev.sh [--all-features | --help]" ]] || \
    fail "--help output changed: $RUN_OUTPUT"

new_case invalid-argument
run_setup --everything
[[ $RUN_STATUS -eq 2 ]] || fail "unknown argument returned $RUN_STATUS instead of 2"
assert_contains "$RUN_OUTPUT" "Usage: scripts/setup-dev.sh [--all-features | --help]"

echo "setup-dev tests passed"
