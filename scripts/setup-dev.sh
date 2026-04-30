#!/usr/bin/env bash
# Install non-cargo build prerequisites (currently just protoc, needed when
# building with `--features full` or any of: etcd, otlp, fluvio).
#
# Idempotent: skips anything already installed.

set -euo pipefail

if command -v protoc >/dev/null 2>&1; then
    echo "protoc already installed: $(protoc --version)"
    exit 0
fi

echo "protoc not found, installing..."

case "$(uname -s)" in
    Linux*)
        if command -v apt-get >/dev/null 2>&1; then
            sudo apt-get update
            sudo apt-get install -y protobuf-compiler
        elif command -v dnf >/dev/null 2>&1; then
            sudo dnf install -y protobuf-compiler
        elif command -v pacman >/dev/null 2>&1; then
            sudo pacman -S --noconfirm protobuf
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
