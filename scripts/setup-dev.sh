#!/usr/bin/env bash
# Install non-cargo build prerequisites. The default installs protoc; pass
# --all-features for the native Aeron and ZeroMQ toolchain as well.
#
# Idempotent: skips anything already installed.

set -euo pipefail

CMAKE_MIN_VERSION=3.30.0
CMAKE_INSTALL_VERSION=3.31.0
# These pins are trusted only after checking Kitware's published
# cmake-${CMAKE_INSTALL_VERSION}-SHA-256.txt; the mocked regression test cannot
# independently authenticate them.
CMAKE_SHA256_AARCH64=45bb6140132427c398437f96bd78820724baa868617470ca40a8c382a8c9e965
CMAKE_SHA256_X86_64=7cfdf4a411c71d13c027199952fd25e8245d85c932ff452e2b9a9e0f6dfe368a
ALL_FEATURES=false

usage() {
    echo "Usage: scripts/setup-dev.sh [--all-features | --help]"
}

run_as_root() {
    if ((EUID == 0)); then
        "$@"
    elif command -v sudo >/dev/null 2>&1; then
        sudo "$@"
    else
        echo "Root privileges are required; install sudo or run this script as root." >&2
        return 1
    fi
}

case $# in
    0) ;;
    1)
        case $1 in
            --all-features) ALL_FEATURES=true ;;
            -h|--help)
                usage
                exit 0
                ;;
            *)
                usage >&2
                exit 2
                ;;
        esac
        ;;
    *)
        usage >&2
        exit 2
        ;;
esac

detect_package_manager() {
    case "$(uname -s)" in
        Linux*)
            if command -v apt-get >/dev/null 2>&1; then
                echo apt-get
            elif command -v dnf >/dev/null 2>&1; then
                echo dnf
            elif command -v pacman >/dev/null 2>&1; then
                echo pacman
            else
                echo "Unsupported Linux distribution. Install prerequisites manually." >&2
                return 1
            fi
            ;;
        Darwin*)
            if command -v brew >/dev/null 2>&1; then
                echo brew
            else
                echo "Homebrew not found. Install it from https://brew.sh, or install prerequisites manually." >&2
                return 1
            fi
            ;;
        *)
            echo "Unsupported OS: $(uname -s). Install prerequisites manually." >&2
            return 1
            ;;
    esac
}

install_protoc() {
    if command -v protoc >/dev/null 2>&1; then
        echo "protoc already installed: $(protoc --version)"
        return
    fi

    echo "protoc not found, installing..."

    case "$(uname -s)" in
        Linux*)
            if command -v apt-get >/dev/null 2>&1; then
                # A third-party PPA that has gone unsigned/403 makes `update`
                # exit non-zero even though the archives we need refreshed
                # fine. Let install decide whether protoc is available.
                run_as_root env DEBIAN_FRONTEND=noninteractive apt-get update || echo "apt-get update reported errors, continuing"
                run_as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y protobuf-compiler
            elif command -v dnf >/dev/null 2>&1; then
                run_as_root dnf install -y protobuf-compiler
            elif command -v pacman >/dev/null 2>&1; then
                run_as_root pacman -S --noconfirm protobuf
            else
                echo "Unsupported Linux distribution. Install protoc manually:"
                echo "  https://github.com/protocolbuffers/protobuf/releases"
                exit 1
            fi
            ;;
        Darwin*)
            if command -v brew >/dev/null 2>&1; then
                brew install protobuf
            else
                echo "Homebrew not found. Install it from https://brew.sh, or install protoc manually:"
                echo "  https://github.com/protocolbuffers/protobuf/releases"
                exit 1
            fi
            ;;
        *)
            echo "Unsupported OS: $(uname -s). Install protoc manually:"
            echo "  https://github.com/protocolbuffers/protobuf/releases"
            exit 1
            ;;
    esac

    echo "Installed: $(protoc --version)"
}

package_installed() {
    local package_manager=$1
    local package=$2
    local status

    case "$package_manager" in
        apt-get)
            status=$(dpkg-query -W -f='${Status}' "$package" 2>/dev/null || true)
            [[ $status == "install ok installed" ]]
            ;;
        dnf) dnf list --installed "$package" >/dev/null 2>&1 ;;
        pacman) pacman -Q "$package" >/dev/null 2>&1 ;;
        brew) brew list --versions "$package" >/dev/null 2>&1 ;;
    esac
}

ensure_dnf_libbsd_repo() {
    local rhel_major

    if package_installed dnf libbsd-devel || dnf list --available libbsd-devel >/dev/null 2>&1; then
        return
    fi

    echo "libbsd-devel is not in the enabled dnf repositories; enabling EPEL"
    if run_as_root dnf install -y epel-release; then
        :
    else
        rhel_major=$(rpm -E '%{rhel}' 2>/dev/null || true)
        if [[ ! $rhel_major =~ ^[0-9]+$ ]]; then
            echo "Unable to determine the Enterprise Linux version needed for EPEL." >&2
            return 1
        fi
        run_as_root dnf install -y \
            "https://dl.fedoraproject.org/pub/epel/epel-release-latest-${rhel_major}.noarch.rpm"
    fi

    if ! dnf list --available libbsd-devel >/dev/null 2>&1; then
        echo "EPEL was enabled, but libbsd-devel is still unavailable." >&2
        return 1
    fi
}

install_native_packages() {
    local package_manager=$1
    local -a packages
    local package
    local all_installed=true

    case "$package_manager" in
        apt-get)
            packages=(build-essential ca-certificates curl clang libclang-dev uuid-dev libbsd-dev cmake libzmq3-dev libssl-dev pkg-config)
            ;;
        dnf)
            packages=(gcc-c++ make ca-certificates curl clang clang-devel libuuid-devel libbsd-devel cmake zeromq-devel openssl-devel pkgconf-pkg-config)
            ;;
        pacman)
            packages=(base-devel ca-certificates curl clang util-linux-libs libbsd cmake zeromq openssl pkgconf)
            ;;
        brew)
            # macOS supplies UUID and BSD APIs; Homebrew provides libclang and
            # the ZeroMQ development files.
            packages=(llvm cmake zeromq openssl@3 pkgconf)
            ;;
    esac

    if [[ $package_manager == dnf ]]; then
        ensure_dnf_libbsd_repo
    fi

    for package in "${packages[@]}"; do
        if ! package_installed "$package_manager" "$package"; then
            all_installed=false
            break
        fi
    done

    if $all_installed; then
        echo "Native packages already installed: ${packages[*]}"
        return
    fi

    echo "Installing native all-features packages: ${packages[*]}"
    case "$package_manager" in
        apt-get)
            run_as_root env DEBIAN_FRONTEND=noninteractive apt-get update || echo "apt-get update reported errors, continuing"
            run_as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y "${packages[@]}"
            ;;
        dnf) run_as_root dnf install -y "${packages[@]}" ;;
        pacman) run_as_root pacman -S --noconfirm "${packages[@]}" ;;
        brew) brew install "${packages[@]}" ;;
    esac
}

cmake_version() {
    local version
    read -r _ _ version < <(cmake --version)
    echo "$version"
}

version_at_least() {
    local current=$1
    local required=$2
    local -a current_parts required_parts
    local current_part required_part
    local i

    IFS=. read -r -a current_parts <<<"$current"
    IFS=. read -r -a required_parts <<<"$required"
    for i in 0 1 2; do
        current_part=${current_parts[$i]:-0}
        required_part=${required_parts[$i]:-0}
        current_part=${current_part%%[!0-9]*}
        required_part=${required_part%%[!0-9]*}
        current_part=${current_part:-0}
        required_part=${required_part:-0}
        if ((10#$current_part > 10#$required_part)); then
            return 0
        elif ((10#$current_part < 10#$required_part)); then
            return 1
        fi
    done
    return 0
}

install_kitware_cmake() {
    local architecture
    local actual_sha256
    local expected_sha256
    local installer
    local url

    case "$(uname -m)" in
        x86_64|amd64)
            architecture=x86_64
            expected_sha256=$CMAKE_SHA256_X86_64
            ;;
        aarch64|arm64)
            architecture=aarch64
            expected_sha256=$CMAKE_SHA256_AARCH64
            ;;
        *)
            echo "No CMake installer is configured for architecture $(uname -m)." >&2
            echo "Install CMake >= $CMAKE_MIN_VERSION manually: https://cmake.org/download/" >&2
            return 1
            ;;
    esac

    installer=$(mktemp)
    url="https://github.com/Kitware/CMake/releases/download/v${CMAKE_INSTALL_VERSION}/cmake-${CMAKE_INSTALL_VERSION}-linux-${architecture}.sh"
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL "$url" -o "$installer"
    elif command -v wget >/dev/null 2>&1; then
        wget -q "$url" -O "$installer"
    else
        echo "curl or wget is required to install CMake $CMAKE_INSTALL_VERSION." >&2
        rm -f "$installer"
        return 1
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        actual_sha256=$(sha256sum "$installer")
    elif command -v shasum >/dev/null 2>&1; then
        actual_sha256=$(shasum -a 256 "$installer")
    else
        echo "sha256sum or shasum is required to verify the CMake installer." >&2
        rm -f "$installer"
        return 1
    fi
    actual_sha256=${actual_sha256%% *}
    if [[ $actual_sha256 != "$expected_sha256" ]]; then
        echo "CMake installer checksum mismatch for linux-${architecture}." >&2
        rm -f "$installer"
        return 1
    fi
    echo "Verified CMake $CMAKE_INSTALL_VERSION installer SHA-256"
    run_as_root sh "$installer" --prefix=/usr/local --skip-license --exclude-subdir
    rm -f "$installer"
}

ensure_recent_cmake() {
    local package_manager=$1
    local installed_version

    if command -v cmake >/dev/null 2>&1; then
        installed_version=$(cmake_version)
        if version_at_least "$installed_version" "$CMAKE_MIN_VERSION"; then
            echo "CMake already installed: $installed_version"
            return
        fi
        echo "CMake $installed_version is too old; installing CMake $CMAKE_INSTALL_VERSION"
    else
        echo "CMake not found; installing CMake $CMAKE_INSTALL_VERSION"
    fi

    if [[ $package_manager == brew ]]; then
        if brew list --versions cmake >/dev/null 2>&1; then
            brew upgrade cmake
        else
            brew install cmake
        fi
    else
        install_kitware_cmake
    fi

    if ! command -v cmake >/dev/null 2>&1; then
        echo "CMake installation completed but cmake is not on PATH." >&2
        return 1
    fi
    installed_version=$(cmake_version)
    if ! version_at_least "$installed_version" "$CMAKE_MIN_VERSION"; then
        echo "CMake $installed_version is still below the required $CMAKE_MIN_VERSION." >&2
        return 1
    fi
    echo "Installed CMake: $installed_version"
}

install_protoc

if ! $ALL_FEATURES; then
    exit 0
fi

PACKAGE_MANAGER=$(detect_package_manager)
install_native_packages "$PACKAGE_MANAGER"
ensure_recent_cmake "$PACKAGE_MANAGER"

clang_version=
read -r clang_version < <(clang --version)
zmq_version=$(pkg-config --modversion libzmq)
echo "Native all-features toolchain ready: $clang_version; libzmq $zmq_version"
if [[ $PACKAGE_MANAGER == brew ]]; then
    echo "Homebrew LLVM is keg-only; if bindgen cannot find libclang, set LIBCLANG_PATH=\"$(brew --prefix llvm)/lib\"."
fi
